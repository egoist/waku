#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("waku-updater is only supported on Linux");
    std::process::exit(1);
}

#[cfg(target_os = "linux")]
fn main() {
    linux::main();
}

#[cfg(target_os = "linux")]
mod linux {
    use std::ffi::{OsStr, OsString};
    use std::fs::{self, File, OpenOptions};
    use std::io::Write as _;
    use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
    use std::path::{Path, PathBuf};
    use std::process::{Child, Command, Stdio};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant};

    use anyhow::{Context as _, bail};

    const MANAGED_MARKER: &str = "share/waku/self-update-v1";
    const MANAGED_MARKER_CONTENTS: &str = "waku-self-update-v1\n";
    const HELPER_EXECUTABLE: &str = "waku-updater";
    const RELAUNCH_READY_ENV: &str = "WAKU_UPDATE_READY_FILE";
    const READY_TIMEOUT: Duration = Duration::from_secs(60);
    const POLL_INTERVAL: Duration = Duration::from_millis(25);
    const MAX_ERROR_LENGTH: usize = 16 * 1024;

    static PATH_NONCE: AtomicU64 = AtomicU64::new(0);

    struct Arguments {
        install_dir: PathBuf,
        staged_dir: PathBuf,
        parent_pid: u32,
    }

    struct Handoff {
        install_dir: PathBuf,
        staged_dir: PathBuf,
        parent: PathBuf,
        prefix_name: String,
        parent_pid: u32,
    }

    enum RelaunchState {
        Ready,
        Exited(String),
        TimedOut,
    }

    pub(super) fn main() {
        let handoff =
            match Arguments::parse(std::env::args_os().skip(1)).and_then(Handoff::validate) {
                Ok(handoff) => handoff,
                Err(error) => {
                    println!("ERROR\t{}", one_line_error(&error));
                    let _ = std::io::stdout().flush();
                    std::process::exit(1);
                }
            };

        println!("READY");
        if std::io::stdout().flush().is_err() {
            std::process::exit(1);
        }
        wait_for_parent(handoff.parent_pid);
        if let Err(error) = apply_update(&handoff) {
            eprintln!("Waku updater: {error:#}");
            std::process::exit(1);
        }
    }

    impl Arguments {
        fn parse(arguments: impl IntoIterator<Item = OsString>) -> anyhow::Result<Self> {
            let mut install_dir = None;
            let mut staged_dir = None;
            let mut parent_pid = None;
            let mut arguments = arguments.into_iter();
            while let Some(argument) = arguments.next() {
                let value = arguments
                    .next()
                    .with_context(|| format!("{} requires a value", argument.to_string_lossy()))?;
                match argument.to_str() {
                    Some("--install-dir") if install_dir.is_none() => {
                        install_dir = Some(PathBuf::from(value));
                    }
                    Some("--staged-dir") if staged_dir.is_none() => {
                        staged_dir = Some(PathBuf::from(value));
                    }
                    Some("--parent-pid") if parent_pid.is_none() => {
                        parent_pid = Some(
                            value
                                .to_str()
                                .context("--parent-pid is not UTF-8")?
                                .parse::<u32>()
                                .context("--parent-pid is not a process id")?,
                        );
                    }
                    _ => bail!(
                        "unknown or repeated argument {}",
                        argument.to_string_lossy()
                    ),
                }
            }
            Ok(Self {
                install_dir: install_dir.context("--install-dir is required")?,
                staged_dir: staged_dir.context("--staged-dir is required")?,
                parent_pid: parent_pid.context("--parent-pid is required")?,
            })
        }
    }

    impl Handoff {
        fn validate(arguments: Arguments) -> anyhow::Result<Self> {
            if unsafe { libc::geteuid() } == 0 {
                bail!("refusing to self-update from a root desktop session");
            }
            if arguments.parent_pid != unsafe { libc::getppid() as u32 } {
                bail!("the update helper was not launched directly by Waku");
            }

            let install_dir = arguments
                .install_dir
                .canonicalize()
                .context("could not resolve the current install")?;
            let staged_dir = arguments
                .staged_dir
                .canonicalize()
                .context("could not resolve the staged install")?;
            let parent = install_dir
                .parent()
                .context("the install has no parent directory")?
                .canonicalize()?;
            let home = dirs::home_dir()
                .context("the user's home directory is unavailable")?
                .canonicalize()?;
            if !install_dir.starts_with(&home) {
                bail!("the install is outside the user's home directory");
            }
            if staged_dir.parent() != Some(parent.as_path()) {
                bail!("the staged install is not beside the current install");
            }
            let prefix_name = install_dir
                .file_name()
                .and_then(OsStr::to_str)
                .context("the install directory has no UTF-8 name")?
                .to_owned();
            let expected_staging_prefix = format!(".{prefix_name}.update-");
            if !staged_dir
                .file_name()
                .and_then(OsStr::to_str)
                .is_some_and(|name| name.starts_with(&expected_staging_prefix))
            {
                bail!("the staged install name does not belong to this Waku prefix");
            }

            validate_packaged_layout(&install_dir)?;
            validate_packaged_layout(&staged_dir)?;
            let running_helper = std::env::current_exe()?.canonicalize()?;
            let expected_helper = install_dir
                .join("bin")
                .join(HELPER_EXECUTABLE)
                .canonicalize()?;
            if running_helper != expected_helper {
                bail!("the helper is not running from the install it was asked to replace");
            }

            Ok(Self {
                install_dir,
                staged_dir,
                parent,
                prefix_name,
                parent_pid: arguments.parent_pid,
            })
        }

        fn unique_path(&self, kind: &str) -> anyhow::Result<PathBuf> {
            for _ in 0..100 {
                let nonce = PATH_NONCE.fetch_add(1, Ordering::Relaxed);
                let candidate = self.parent.join(format!(
                    ".{}.{kind}-{}-{nonce}",
                    self.prefix_name,
                    std::process::id()
                ));
                match fs::symlink_metadata(&candidate) {
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        return Ok(candidate);
                    }
                    Ok(_) => continue,
                    Err(error) => return Err(error.into()),
                }
            }
            bail!("could not reserve a unique {kind} path")
        }

        fn error_path(&self) -> PathBuf {
            self.parent
                .join(format!(".{}.update-error", self.prefix_name))
        }
    }

    fn apply_update(handoff: &Handoff) -> anyhow::Result<()> {
        let backup = handoff.unique_path("update-backup")?;
        if let Err(error) = fs::rename(&handoff.install_dir, &backup) {
            let message = format!("could not preserve the current Waku install: {error}");
            record_error(handoff, &message);
            let _ = launch(&handoff.install_dir, None);
            bail!(message);
        }
        sync_directory(&handoff.parent);

        if let Err(error) = fs::rename(&handoff.staged_dir, &handoff.install_dir) {
            fs::rename(&backup, &handoff.install_dir).with_context(|| {
                format!(
                    "the staged install could not be activated ({error}) and the previous install could not be restored"
                )
            })?;
            sync_directory(&handoff.parent);
            let message = format!("could not activate the staged Waku install: {error}");
            record_error(handoff, &message);
            let _ = launch(&handoff.install_dir, None);
            bail!(message);
        }
        sync_directory(&handoff.parent);

        let ready_path = match handoff.unique_path("update-ready") {
            Ok(path) => path,
            Err(error) => {
                let message = format!("could not reserve the update startup handshake: {error}");
                rollback(handoff, &backup, &message)?;
                bail!(message);
            }
        };
        let mut replacement = match launch(&handoff.install_dir, Some(&ready_path)) {
            Ok(child) => child,
            Err(error) => {
                let message = format!("the updated Waku build could not start: {error}");
                rollback(handoff, &backup, &message)?;
                bail!(message);
            }
        };

        match wait_for_relaunch(&mut replacement, &ready_path) {
            RelaunchState::Ready => {
                let _ = fs::remove_file(&ready_path);
                if let Err(error) = fs::remove_dir_all(&backup) {
                    eprintln!(
                        "Waku updater: update succeeded but the rollback copy {} could not be removed: {error}",
                        backup.display()
                    );
                }
                sync_directory(&handoff.parent);
                Ok(())
            }
            RelaunchState::Exited(status) => {
                let message =
                    format!("the updated Waku build exited before its window opened ({status})");
                rollback(handoff, &backup, &message)?;
                bail!(message)
            }
            RelaunchState::TimedOut => {
                // Never replace a running process underneath it. The new app
                // remains active and the exact rollback directory is retained
                // for manual recovery instead of being destructively guessed.
                eprintln!(
                    "Waku updater: the new build stayed alive but did not acknowledge startup; retaining {}",
                    backup.display()
                );
                Ok(())
            }
        }
    }

    fn rollback(handoff: &Handoff, backup: &Path, message: &str) -> anyhow::Result<()> {
        let failed = handoff.unique_path("failed-update")?;
        fs::rename(&handoff.install_dir, &failed)
            .context("could not move the failed update out of the install path")?;
        if let Err(error) = fs::rename(backup, &handoff.install_dir) {
            let _ = fs::rename(&failed, &handoff.install_dir);
            return Err(error).context("could not restore the previous Waku install");
        }
        sync_directory(&handoff.parent);
        let _ = fs::remove_dir_all(&failed);
        record_error(handoff, message);
        launch(&handoff.install_dir, None)
            .context("the previous Waku build was restored but could not be relaunched")?;
        Ok(())
    }

    fn launch(prefix: &Path, ready_path: Option<&Path>) -> std::io::Result<Child> {
        let mut command = Command::new(prefix.join("bin/waku"));
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if let Some(ready_path) = ready_path {
            command.env(RELAUNCH_READY_ENV, ready_path);
        }
        command.spawn()
    }

    fn wait_for_parent(parent_pid: u32) {
        while unsafe { libc::getppid() as u32 } == parent_pid {
            std::thread::sleep(POLL_INTERVAL);
        }
    }

    fn wait_for_relaunch(child: &mut Child, ready_path: &Path) -> RelaunchState {
        let deadline = Instant::now() + READY_TIMEOUT;
        loop {
            if ready_path.is_file() {
                return RelaunchState::Ready;
            }
            match child.try_wait() {
                Ok(Some(status)) => return RelaunchState::Exited(status.to_string()),
                Ok(None) => {}
                Err(error) => {
                    eprintln!("Waku updater: could not observe the relaunched app: {error}");
                    return RelaunchState::TimedOut;
                }
            }
            if Instant::now() >= deadline {
                return RelaunchState::TimedOut;
            }
            std::thread::sleep(POLL_INTERVAL);
        }
    }

    fn validate_packaged_layout(prefix: &Path) -> anyhow::Result<()> {
        let metadata = fs::symlink_metadata(prefix)?;
        if !metadata.file_type().is_dir() {
            bail!("the Waku prefix is not a real directory");
        }
        let marker = prefix.join(MANAGED_MARKER);
        let marker_metadata = fs::symlink_metadata(&marker)?;
        if !marker_metadata.file_type().is_file()
            || fs::read_to_string(&marker).ok().as_deref() != Some(MANAGED_MARKER_CONTENTS)
        {
            bail!("the install is not marked as a Waku-managed tarball");
        }
        for executable in ["waku", "waku-daemon", HELPER_EXECUTABLE] {
            let path = prefix.join("bin").join(executable);
            let metadata = fs::symlink_metadata(&path)?;
            if !metadata.file_type().is_file() || metadata.permissions().mode() & 0o111 == 0 {
                bail!("the install is missing executable bin/{executable}");
            }
        }
        Ok(())
    }

    fn record_error(handoff: &Handoff, message: &str) {
        let path = handoff.error_path();
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_dir() => return,
            Ok(_) => {
                let _ = fs::remove_file(&path);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return,
        }
        let _ = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .and_then(|mut file| {
                let mut end = message.len().min(MAX_ERROR_LENGTH);
                while !message.is_char_boundary(end) {
                    end -= 1;
                }
                file.write_all(&message.as_bytes()[..end])?;
                file.write_all(b"\n")?;
                file.sync_all()
            });
    }

    fn sync_directory(directory: &Path) {
        let _ = File::open(directory).and_then(|file| file.sync_all());
    }

    fn one_line_error(error: &anyhow::Error) -> String {
        format!("{error:#}")
            .replace(['\r', '\n', '\t'], " ")
            .chars()
            .take(1024)
            .collect()
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn temporary_directory(label: &str) -> PathBuf {
            let path = std::env::temp_dir().join(format!(
                "waku-update-helper-{label}-{}-{}",
                std::process::id(),
                PATH_NONCE.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            path
        }

        fn write_layout(prefix: &Path, value: &[u8]) {
            fs::create_dir_all(prefix.join("bin")).unwrap();
            fs::create_dir_all(prefix.join("share/waku")).unwrap();
            fs::write(prefix.join(MANAGED_MARKER), MANAGED_MARKER_CONTENTS).unwrap();
            for executable in ["waku", "waku-daemon", HELPER_EXECUTABLE] {
                let path = prefix.join("bin").join(executable);
                fs::write(&path, value).unwrap();
                let mut permissions = fs::metadata(&path).unwrap().permissions();
                permissions.set_mode(0o755);
                fs::set_permissions(path, permissions).unwrap();
            }
        }

        #[test]
        fn arguments_reject_repetition_and_missing_values() {
            assert!(
                Arguments::parse([
                    "--install-dir".into(),
                    "/tmp/one".into(),
                    "--install-dir".into(),
                    "/tmp/two".into(),
                ])
                .is_err()
            );
            assert!(Arguments::parse(["--staged-dir".into()]).is_err());
        }

        #[test]
        fn packaged_layout_requires_real_executables_and_marker() {
            let directory = temporary_directory("layout");
            let prefix = directory.join("waku.app");
            write_layout(&prefix, b"old");
            validate_packaged_layout(&prefix).unwrap();
            fs::remove_file(prefix.join("bin/waku-daemon")).unwrap();
            assert!(validate_packaged_layout(&prefix).is_err());
            fs::remove_dir_all(directory).unwrap();
        }

        #[test]
        fn directory_swap_can_be_rolled_back_without_merging() {
            let directory = temporary_directory("swap");
            let install = directory.join("waku.app");
            let staged = directory.join(".waku.app.update-test");
            let backup = directory.join(".waku.app.backup-test");
            write_layout(&install, b"old");
            write_layout(&staged, b"new");

            fs::rename(&install, &backup).unwrap();
            fs::rename(&staged, &install).unwrap();
            assert_eq!(fs::read(install.join("bin/waku")).unwrap(), b"new");
            let failed = directory.join(".waku.app.failed-test");
            fs::rename(&install, &failed).unwrap();
            fs::rename(&backup, &install).unwrap();
            assert_eq!(fs::read(install.join("bin/waku")).unwrap(), b"old");

            fs::remove_dir_all(directory).unwrap();
        }

        #[test]
        fn full_handoff_waits_for_startup_and_removes_the_rollback_copy() {
            let directory = temporary_directory("handoff");
            let install = directory.join("waku.app");
            let staged = directory.join(".waku.app.update-test");
            write_layout(&install, b"old");
            write_layout(&staged, b"new");
            let replacement = staged.join("bin/waku");
            fs::write(
                &replacement,
                b"#!/bin/sh\nprintf 'ready\\n' > \"$WAKU_UPDATE_READY_FILE\"\n",
            )
            .unwrap();
            let mut permissions = fs::metadata(&replacement).unwrap().permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&replacement, permissions).unwrap();
            let handoff = Handoff {
                install_dir: install.clone(),
                staged_dir: staged,
                parent: directory.clone(),
                prefix_name: "waku.app".into(),
                parent_pid: std::process::id(),
            };

            apply_update(&handoff).unwrap();

            assert!(install.join("bin/waku").is_file());
            assert_eq!(fs::read(install.join("bin/waku-daemon")).unwrap(), b"new");
            assert!(
                fs::read_dir(&directory).unwrap().all(|entry| {
                    !entry
                        .unwrap()
                        .file_name()
                        .to_string_lossy()
                        .contains("update-backup")
                }),
                "the acknowledged build should release its rollback directory"
            );
            fs::remove_dir_all(directory).unwrap();
        }
    }
}

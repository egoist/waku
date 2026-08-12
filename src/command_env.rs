use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{OnceLock, RwLock};
use std::time::{Duration, Instant};

#[cfg(target_os = "macos")]
use std::ffi::CStr;
#[cfg(target_os = "macos")]
use std::mem::MaybeUninit;
#[cfg(target_os = "macos")]
use std::os::unix::ffi::OsStringExt;
#[cfg(target_os = "macos")]
use std::os::unix::fs::OpenOptionsExt;
#[cfg(target_os = "macos")]
use std::os::unix::process::CommandExt;

const LOGIN_SHELL_PATH_TIMEOUT: Duration = Duration::from_secs(5);
const INTERACTIVE_SHELL_PATH_TIMEOUT: Duration = Duration::from_secs(3);
const SHELL_PATH_COMMAND: &str = "/usr/bin/printenv PATH > \"$WAKU_SHELL_PATH_CAPTURE_FILE\"";

static LOGIN_SHELL_PATH: OnceLock<RwLock<Option<OsString>>> = OnceLock::new();
static SHELL_PATH_CAPTURE_ID: AtomicU64 = AtomicU64::new(0);

/// Build a command with the executable search path a terminal-launched Waku
/// normally inherits. Apps opened through LaunchServices only receive the
/// system PATH, which is not enough for script-based CLIs whose shebang uses
/// `/usr/bin/env` (for example, an npm-installed Codex launcher needs `node`).
/// The cached interactive login-shell PATH comes first so nvm/fnm select the
/// same runtime the user gets in a normal terminal.
pub fn command(program: impl AsRef<OsStr>) -> Command {
    let mut command = Command::new(program);
    if let Ok(path) = std::env::join_paths(executable_search_paths()) {
        command.env("PATH", path);
    }
    command
}

pub fn find_executable(name: &str) -> Option<PathBuf> {
    let candidate = Path::new(name);
    if candidate.components().count() > 1 && candidate.is_file() {
        return Some(candidate.to_path_buf());
    }
    executable_search_paths()
        .into_iter()
        .map(|directory| directory.join(name))
        .find(|candidate| candidate.is_file())
}

/// Resolve a user-supplied binary override: `~` expands to the home
/// directory, a path must point at an existing file, and a bare name searches
/// the same directories as [`find_executable`].
pub fn resolve_binary_override(spec: &str) -> Option<PathBuf> {
    let spec = spec.trim();
    if spec.is_empty() {
        return None;
    }
    if let Some(rest) = spec.strip_prefix("~/") {
        let candidate = dirs::home_dir()?.join(rest);
        return candidate.is_file().then_some(candidate);
    }
    find_executable(spec)
}

pub fn executable_search_path() -> Option<std::ffi::OsString> {
    std::env::join_paths(executable_search_paths()).ok()
}

/// Resolve PATH through the user's interactive login shell and cache it for
/// provider discovery and every later child process. This starts a shell and
/// must therefore only be called from a background thread.
pub fn refresh_from_default_shell() -> bool {
    let Some(path) = resolve_default_shell_path(LOGIN_SHELL_PATH_TIMEOUT) else {
        return false;
    };
    *login_shell_path()
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(path);
    true
}

fn executable_search_paths() -> Vec<PathBuf> {
    search_paths_from(
        cached_login_shell_path().as_deref(),
        std::env::var_os("PATH").as_deref(),
        dirs::home_dir().as_deref(),
    )
}

fn login_shell_path() -> &'static RwLock<Option<OsString>> {
    LOGIN_SHELL_PATH.get_or_init(|| RwLock::new(None))
}

fn cached_login_shell_path() -> Option<OsString> {
    login_shell_path()
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

fn search_paths_from(
    shell_path: Option<&OsStr>,
    inherited_path: Option<&OsStr>,
    home: Option<&Path>,
) -> Vec<PathBuf> {
    let mut directories = Vec::new();
    for path in [shell_path, inherited_path].into_iter().flatten() {
        directories.extend(std::env::split_paths(path));
    }
    if let Some(home) = home {
        directories.extend([
            home.join(".local/bin"),
            home.join(".bun/bin"),
            home.join(".cargo/bin"),
            home.join(".local/share/mise/shims"),
            home.join(".volta/bin"),
        ]);
    }
    directories.extend([
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
        PathBuf::from("/usr/bin"),
        PathBuf::from("/bin"),
        PathBuf::from("/usr/sbin"),
        PathBuf::from("/sbin"),
    ]);

    let mut seen = HashSet::new();
    directories.retain(|directory| seen.insert(directory.clone()));
    directories
}

fn resolve_default_shell_path(timeout: Duration) -> Option<OsString> {
    let started_at = Instant::now();
    for shell in default_shell_candidates() {
        for shell_args in [["-i", "-l", "-c"].as_slice(), ["-l", "-c"].as_slice()] {
            let remaining = timeout.checked_sub(started_at.elapsed())?;
            if remaining.is_zero() {
                return None;
            }
            // Leave part of the total budget for a non-interactive login-shell
            // fallback when an interactive rc file blocks or exits early.
            let attempt_timeout = if shell_args.first() == Some(&"-i") {
                remaining.min(INTERACTIVE_SHELL_PATH_TIMEOUT)
            } else {
                remaining
            };
            if let Some(path) = capture_shell_path(&shell, shell_args, attempt_timeout) {
                return Some(path);
            }
        }
    }
    None
}

fn default_shell_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(shell) = std::env::var_os("SHELL").filter(|shell| !shell.is_empty()) {
        candidates.push(PathBuf::from(shell));
    }
    #[cfg(target_os = "macos")]
    if let Some(shell) = account_default_shell() {
        candidates.push(shell);
    }
    candidates.push(PathBuf::from("/bin/zsh"));

    let mut seen = HashSet::new();
    candidates.retain(|shell| seen.insert(shell.clone()));
    candidates
}

#[cfg(target_os = "macos")]
fn account_default_shell() -> Option<PathBuf> {
    let suggested_size = unsafe { libc::sysconf(libc::_SC_GETPW_R_SIZE_MAX) };
    let mut buffer_size = if suggested_size > 0 {
        suggested_size as usize
    } else {
        16 * 1024
    };
    loop {
        let mut passwd = MaybeUninit::<libc::passwd>::uninit();
        let mut result = std::ptr::null_mut();
        let mut buffer = vec![0_u8; buffer_size];
        let status = unsafe {
            libc::getpwuid_r(
                libc::geteuid(),
                passwd.as_mut_ptr(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                &mut result,
            )
        };
        if status == libc::ERANGE && buffer_size < 1024 * 1024 {
            buffer_size *= 2;
            continue;
        }
        if status != 0 || result.is_null() {
            return None;
        }
        let shell = unsafe { (*result).pw_shell };
        if shell.is_null() {
            return None;
        }
        let bytes = unsafe { CStr::from_ptr(shell) }.to_bytes();
        return (!bytes.is_empty()).then(|| PathBuf::from(OsString::from_vec(bytes.to_vec())));
    }
}

fn capture_shell_path(shell: &Path, shell_args: &[&str], timeout: Duration) -> Option<OsString> {
    let capture = ShellPathCapture::create()?;
    let mut command = Command::new(shell);
    command
        .args(shell_args)
        .arg(SHELL_PATH_COMMAND)
        .env("WAKU_SHELL_PATH_CAPTURE_FILE", capture.path())
        // Match shell-env's safeguards for common interactive zsh setups so
        // an update prompt or tmux auto-start cannot consume the probe budget.
        .env("DISABLE_AUTO_UPDATE", "true")
        .env("ZSH_TMUX_AUTOSTARTED", "true")
        .env("ZSH_TMUX_AUTOSTART", "false")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(target_os = "macos")]
    command.process_group(0);

    let mut child = command.spawn().ok()?;
    if !wait_for_child(&mut child, timeout) {
        return None;
    }
    let mut bytes = fs::read(capture.path()).ok()?;
    while matches!(bytes.last(), Some(b'\n' | b'\r')) {
        bytes.pop();
    }
    if bytes.is_empty() {
        return None;
    }
    #[cfg(target_os = "macos")]
    return Some(OsString::from_vec(bytes));
    #[cfg(not(target_os = "macos"))]
    return String::from_utf8(bytes).ok().map(OsString::from);
}

fn wait_for_child(child: &mut Child, timeout: Duration) -> bool {
    let started_at = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if started_at.elapsed() < timeout => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(None) | Err(_) => {
                terminate_shell_capture(child);
                return false;
            }
        }
    }
}

fn terminate_shell_capture(child: &mut Child) {
    #[cfg(target_os = "macos")]
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGKILL);
    }
    let _ = child.kill();
    let _ = child.wait();
}

struct ShellPathCapture(PathBuf);

impl ShellPathCapture {
    fn create() -> Option<Self> {
        for _ in 0..16 {
            let id = SHELL_PATH_CAPTURE_ID.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!(".waku-shell-path-{}-{id}", std::process::id()));
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(target_os = "macos")]
            options.mode(0o600);
            match options.open(&path) {
                Ok(_) => return Some(Self(path)),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(_) => return None,
            }
        }
        None
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for ShellPathCapture {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn launch_services_path_is_extended_for_script_based_clis() {
        let home = Path::new("/Users/example");
        let paths = search_paths_from(None, Some(OsStr::new("/usr/bin:/bin")), Some(home));

        assert_eq!(paths[0], PathBuf::from("/usr/bin"));
        assert_eq!(paths[1], PathBuf::from("/bin"));
        assert!(paths.contains(&home.join(".bun/bin")));
        assert!(paths.contains(&home.join(".local/share/mise/shims")));
        assert!(paths.contains(&PathBuf::from("/opt/homebrew/bin")));
        assert_eq!(
            paths
                .iter()
                .filter(|path| *path == Path::new("/bin"))
                .count(),
            1
        );
    }

    #[test]
    fn login_shell_path_precedes_the_inherited_desktop_path() {
        let paths = search_paths_from(
            Some(OsStr::new(
                "/Users/example/.nvm/versions/node/v22.0.0/bin:/Users/example/.local/share/fnm/current/bin",
            )),
            Some(OsStr::new("/usr/bin:/bin")),
            None,
        );

        assert_eq!(
            paths[..4],
            [
                PathBuf::from("/Users/example/.nvm/versions/node/v22.0.0/bin"),
                PathBuf::from("/Users/example/.local/share/fnm/current/bin"),
                PathBuf::from("/usr/bin"),
                PathBuf::from("/bin"),
            ]
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn captures_path_from_a_shell_process() {
        let id = SHELL_PATH_CAPTURE_ID.fetch_add(1, Ordering::Relaxed);
        let directory =
            std::env::temp_dir().join(format!("waku-command-env-test-{}-{id}", std::process::id()));
        fs::create_dir(&directory).expect("create shell fixture directory");
        let shell = directory.join("fake-shell");
        fs::write(
            &shell,
            "#!/bin/sh\nprintf '%s\\n' '/Users/example/.fnm/current/bin:/usr/bin' > \"$WAKU_SHELL_PATH_CAPTURE_FILE\"\n",
        )
        .expect("write shell fixture");
        let mut permissions = fs::metadata(&shell)
            .expect("read shell fixture")
            .permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&shell, permissions).expect("make shell fixture executable");

        let path = capture_shell_path(&shell, &["-i", "-l", "-c"], Duration::from_secs(1));

        assert_eq!(
            path.as_deref(),
            Some(OsStr::new("/Users/example/.fnm/current/bin:/usr/bin"))
        );
        let _ = fs::remove_file(shell);
        let _ = fs::remove_dir(directory);
    }
}

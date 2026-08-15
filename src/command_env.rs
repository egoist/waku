use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::fs::{self, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{OnceLock, RwLock};
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::ffi::CStr;
#[cfg(unix)]
use std::mem::MaybeUninit;
#[cfg(unix)]
use std::os::unix::ffi::OsStringExt;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
#[cfg(unix)]
use std::os::unix::process::CommandExt;

const LOGIN_SHELL_ENV_TIMEOUT: Duration = Duration::from_secs(5);
const INTERACTIVE_SHELL_ENV_TIMEOUT: Duration = Duration::from_secs(3);
const SHELL_ENV_COMMAND: &str = "/usr/bin/env -0 > \"$WAKU_SHELL_ENV_CAPTURE_FILE\"";

type ShellEnvironment = Vec<(OsString, OsString)>;

static LOGIN_SHELL_ENVIRONMENT: OnceLock<RwLock<Option<ShellEnvironment>>> = OnceLock::new();
static SHELL_ENV_CAPTURE_ID: AtomicU64 = AtomicU64::new(0);

/// Build a command with the environment a terminal-launched Waku normally
/// inherits. Apps opened through LaunchServices do not receive variables
/// exported by the user's shell, including the PATH needed by script-based
/// CLIs whose shebang uses `/usr/bin/env` (for example, an npm-installed Codex
/// launcher needs `node`). Callers can add provider-specific overrides after
/// this.
pub fn command(program: impl AsRef<OsStr>) -> Command {
    let mut command = Command::new(program);
    command.envs(shell_environment());
    command
}

/// Spawn `command` with `SIGCHLD` unblocked in the child. On macOS, libdispatch
/// worker threads (which back GPUI's background executor) block `SIGCHLD`, and
/// a process spawned from such a thread inherits the blocked mask. That breaks
/// provider-side async process reapers. The caller's mask is restored as soon
/// as the child has been created.
pub(crate) fn spawn(command: &mut Command) -> io::Result<Child> {
    with_sigchld_unblocked(|| command.spawn())
}

/// Spawn `command` through [`spawn`] and collect its output.
///
/// `Command::spawn` inherits standard streams by default, unlike
/// `Command::output`. Own all three streams here so callers keep the latter's
/// behavior while the signal mask is changed only for the spawn itself.
pub(crate) fn output(command: &mut Command) -> io::Result<Output> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    spawn(command)?.wait_with_output()
}

/// Normalize a Waku-owned provider thread before a dependency spawns the child
/// internally. The ACP SDK owns its `async_process::Command`, so its dedicated
/// connection thread uses this once at startup instead of [`spawn`].
pub(crate) fn unblock_sigchld_for_current_thread() -> io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        let sigchld = sigchld_set()?;
        pthread_result(unsafe {
            libc::pthread_sigmask(libc::SIG_UNBLOCK, &sigchld, std::ptr::null_mut())
        })
    }
    #[cfg(not(target_os = "macos"))]
    {
        Ok(())
    }
}

fn with_sigchld_unblocked<T>(operation: impl FnOnce() -> io::Result<T>) -> io::Result<T> {
    #[cfg(target_os = "macos")]
    let _restore = SignalMaskRestore::unblock_sigchld()?;
    operation()
}

#[cfg(target_os = "macos")]
fn sigchld_set() -> io::Result<libc::sigset_t> {
    let mut set = MaybeUninit::<libc::sigset_t>::uninit();
    if unsafe { libc::sigemptyset(set.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let mut set = unsafe { set.assume_init() };
    if unsafe { libc::sigaddset(&mut set, libc::SIGCHLD) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(set)
}

#[cfg(target_os = "macos")]
fn pthread_result(status: libc::c_int) -> io::Result<()> {
    if status == 0 {
        Ok(())
    } else {
        // pthread APIs return the error number directly instead of setting
        // errno, so `last_os_error` would report unrelated thread state.
        Err(io::Error::from_raw_os_error(status))
    }
}

#[cfg(target_os = "macos")]
struct SignalMaskRestore(libc::sigset_t);

#[cfg(target_os = "macos")]
impl SignalMaskRestore {
    fn unblock_sigchld() -> io::Result<Self> {
        let sigchld = sigchld_set()?;
        let mut previous = MaybeUninit::<libc::sigset_t>::uninit();
        pthread_result(unsafe {
            libc::pthread_sigmask(libc::SIG_UNBLOCK, &sigchld, previous.as_mut_ptr())
        })?;
        Ok(Self(unsafe { previous.assume_init() }))
    }
}

#[cfg(target_os = "macos")]
impl Drop for SignalMaskRestore {
    fn drop(&mut self) {
        let _ = unsafe { libc::pthread_sigmask(libc::SIG_SETMASK, &self.0, std::ptr::null_mut()) };
    }
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

/// Resolve the user's interactive login-shell environment and cache it for
/// provider discovery and every later child process. This starts a shell and
/// must therefore only be called from a background thread.
pub fn refresh_from_default_shell() -> bool {
    let Some(environment) = resolve_default_shell_environment(LOGIN_SHELL_ENV_TIMEOUT) else {
        return false;
    };
    *login_shell_environment()
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(environment);
    true
}

fn executable_search_paths() -> Vec<PathBuf> {
    search_paths_from(
        cached_login_shell_variable(OsStr::new("PATH")).as_deref(),
        std::env::var_os("PATH").as_deref(),
        dirs::home_dir().as_deref(),
    )
}

fn login_shell_environment() -> &'static RwLock<Option<ShellEnvironment>> {
    LOGIN_SHELL_ENVIRONMENT.get_or_init(|| RwLock::new(None))
}

pub(crate) fn shell_environment() -> ShellEnvironment {
    login_shell_environment()
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
        .unwrap_or_default()
}

fn cached_login_shell_variable(name: &OsStr) -> Option<OsString> {
    login_shell_environment()
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .as_ref()?
        .iter()
        .find(|(candidate, _)| candidate == name)
        .map(|(_, value)| value.clone())
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

fn resolve_default_shell_environment(timeout: Duration) -> Option<ShellEnvironment> {
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
                remaining.min(INTERACTIVE_SHELL_ENV_TIMEOUT)
            } else {
                remaining
            };
            if let Some(environment) =
                capture_shell_environment(&shell, shell_args, attempt_timeout)
            {
                return Some(environment);
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
    #[cfg(unix)]
    if let Some(shell) = account_default_shell() {
        candidates.push(shell);
    }
    #[cfg(target_os = "macos")]
    candidates.push(PathBuf::from("/bin/zsh"));
    #[cfg(target_os = "linux")]
    candidates.extend([PathBuf::from("/bin/bash"), PathBuf::from("/bin/sh")]);

    let mut seen = HashSet::new();
    candidates.retain(|shell| seen.insert(shell.clone()));
    candidates
}

/// Pick the user's configured login shell for an interactive terminal, with a
/// platform shell as a final fallback when desktop launchers omit `SHELL`.
pub fn default_terminal_shell() -> PathBuf {
    default_shell_candidates()
        .into_iter()
        .find(|shell| shell.is_file())
        .unwrap_or_else(|| PathBuf::from("/bin/sh"))
}

#[cfg(unix)]
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

fn capture_shell_environment(
    shell: &Path,
    shell_args: &[&str],
    timeout: Duration,
) -> Option<ShellEnvironment> {
    let capture = ShellEnvironmentCapture::create()?;
    let mut command = Command::new(shell);
    command
        .args(shell_args)
        .arg(SHELL_ENV_COMMAND)
        .env("WAKU_SHELL_ENV_CAPTURE_FILE", capture.path())
        // Match shell-env's safeguards for common interactive zsh setups so
        // an update prompt or tmux auto-start cannot consume the probe budget.
        .env("DISABLE_AUTO_UPDATE", "true")
        .env("ZSH_TMUX_AUTOSTARTED", "true")
        .env("ZSH_TMUX_AUTOSTART", "false")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    command.process_group(0);

    let mut child = spawn(&mut command).ok()?;
    if !wait_for_child(&mut child, timeout) {
        return None;
    }
    parse_shell_environment(&fs::read(capture.path()).ok()?)
}

fn parse_shell_environment(bytes: &[u8]) -> Option<ShellEnvironment> {
    let environment = bytes
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
        .filter_map(|entry| {
            let separator = entry.iter().position(|byte| *byte == b'=')?;
            if separator == 0 {
                return None;
            }
            let name = os_string_from_bytes(&entry[..separator])?;
            if is_shell_capture_variable(&name) {
                return None;
            }
            let value = os_string_from_bytes(&entry[separator + 1..])?;
            Some((name, value))
        })
        .collect::<Vec<_>>();
    (!environment.is_empty()).then_some(environment)
}

fn is_shell_capture_variable(name: &OsStr) -> bool {
    [
        "WAKU_SHELL_ENV_CAPTURE_FILE",
        "DISABLE_AUTO_UPDATE",
        "ZSH_TMUX_AUTOSTARTED",
        "ZSH_TMUX_AUTOSTART",
    ]
    .into_iter()
    .any(|candidate| name == OsStr::new(candidate))
}

#[cfg(unix)]
fn os_string_from_bytes(bytes: &[u8]) -> Option<OsString> {
    Some(OsString::from_vec(bytes.to_vec()))
}

#[cfg(not(unix))]
fn os_string_from_bytes(bytes: &[u8]) -> Option<OsString> {
    String::from_utf8(bytes.to_vec()).ok().map(OsString::from)
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
    #[cfg(unix)]
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGKILL);
    }
    let _ = child.kill();
    let _ = child.wait();
}

struct ShellEnvironmentCapture(PathBuf);

impl ShellEnvironmentCapture {
    fn create() -> Option<Self> {
        for _ in 0..16 {
            let id = SHELL_ENV_CAPTURE_ID.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!(".waku-shell-env-{}-{id}", std::process::id()));
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
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

impl Drop for ShellEnvironmentCapture {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn output_captures_stdout_and_stderr() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "printf stdout; printf stderr >&2"]);

        let output = output(&mut command).expect("command should run");

        assert_eq!(output.stdout, b"stdout");
        assert_eq!(output.stderr, b"stderr");
    }

    #[cfg(target_os = "macos")]
    fn sigchld_is_blocked() -> io::Result<bool> {
        let mut current = MaybeUninit::<libc::sigset_t>::uninit();
        pthread_result(unsafe {
            libc::pthread_sigmask(libc::SIG_SETMASK, std::ptr::null(), current.as_mut_ptr())
        })?;
        Ok(unsafe { libc::sigismember(current.as_ptr(), libc::SIGCHLD) } == 1)
    }

    #[cfg(target_os = "macos")]
    fn block_sigchld() -> io::Result<SignalMaskRestore> {
        let sigchld = sigchld_set()?;
        let mut previous = MaybeUninit::<libc::sigset_t>::uninit();
        pthread_result(unsafe {
            libc::pthread_sigmask(libc::SIG_BLOCK, &sigchld, previous.as_mut_ptr())
        })?;
        Ok(SignalMaskRestore(unsafe { previous.assume_init() }))
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn spawn_unblocks_sigchld_in_the_child_and_restores_the_caller() {
        if std::env::var_os("WAKU_SIGCHLD_CHILD_PROBE").is_some() {
            assert!(!sigchld_is_blocked().expect("read child signal mask"));
            return;
        }

        let _restore_original = block_sigchld().expect("block SIGCHLD for the fixture");
        assert!(sigchld_is_blocked().expect("read blocked parent mask"));

        let mut command = Command::new(std::env::current_exe().expect("resolve test executable"));
        command
            .args([
                "--exact",
                "command_env::tests::spawn_unblocks_sigchld_in_the_child_and_restores_the_caller",
                "--nocapture",
            ])
            .env("WAKU_SIGCHLD_CHILD_PROBE", "1");
        let output = output(&mut command).expect("spawn child signal probe");

        assert!(
            output.status.success(),
            "child signal probe failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(sigchld_is_blocked().expect("read restored parent mask"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn dedicated_provider_thread_can_normalize_sigchld() {
        let _restore_original = block_sigchld().expect("block SIGCHLD for the fixture");

        unblock_sigchld_for_current_thread().expect("unblock provider thread");

        assert!(!sigchld_is_blocked().expect("read normalized signal mask"));
    }

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

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_shell_candidates_have_system_fallbacks() {
        let candidates = default_shell_candidates();

        assert!(candidates.contains(&PathBuf::from("/bin/bash")));
        assert!(candidates.contains(&PathBuf::from("/bin/sh")));
        assert!(default_terminal_shell().is_file());
    }

    #[cfg(unix)]
    #[test]
    fn parses_null_delimited_environment_without_losing_value_contents() {
        let environment = parse_shell_environment(
            b"PATH=/Users/example/.fnm/current/bin:/usr/bin\0TOKEN=line one\nline two=rest\0EMPTY=\0WAKU_SHELL_ENV_CAPTURE_FILE=/tmp/capture\0",
        )
        .expect("parse shell environment");

        assert_eq!(
            environment,
            vec![
                (
                    OsString::from("PATH"),
                    OsString::from("/Users/example/.fnm/current/bin:/usr/bin"),
                ),
                (
                    OsString::from("TOKEN"),
                    OsString::from("line one\nline two=rest"),
                ),
                (OsString::from("EMPTY"), OsString::new()),
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn captures_environment_from_a_shell_process() {
        let id = SHELL_ENV_CAPTURE_ID.fetch_add(1, Ordering::Relaxed);
        let directory =
            std::env::temp_dir().join(format!("waku-command-env-test-{}-{id}", std::process::id()));
        fs::create_dir(&directory).expect("create shell fixture directory");
        let shell = directory.join("fake-shell");
        fs::write(
            &shell,
            "#!/bin/sh\n/usr/bin/printf 'PATH=/Users/example/.fnm/current/bin:/usr/bin\\000WAKU_TEST_TOKEN=from-shell\\000' > \"$WAKU_SHELL_ENV_CAPTURE_FILE\"\n",
        )
        .expect("write shell fixture");
        let mut permissions = fs::metadata(&shell)
            .expect("read shell fixture")
            .permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&shell, permissions).expect("make shell fixture executable");

        let environment =
            capture_shell_environment(&shell, &["-i", "-l", "-c"], LOGIN_SHELL_ENV_TIMEOUT)
                .expect("capture shell environment");

        assert_eq!(
            environment,
            vec![
                (
                    OsString::from("PATH"),
                    OsString::from("/Users/example/.fnm/current/bin:/usr/bin"),
                ),
                (
                    OsString::from("WAKU_TEST_TOKEN"),
                    OsString::from("from-shell"),
                ),
            ]
        );
        let _ = fs::remove_file(shell);
        let _ = fs::remove_dir(directory);
    }
}

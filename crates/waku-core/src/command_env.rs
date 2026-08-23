//! Process environment capture and provider-safe command spawning.

use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::{OnceLock, RwLock};

use std::fs::{self, OpenOptions};
use std::sync::atomic::{AtomicU64, Ordering};
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
#[cfg(unix)]
const INTERACTIVE_SHELL_ENV_TIMEOUT: Duration = Duration::from_secs(3);
#[cfg(unix)]
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
    let program = program.as_ref();
    let search_path = child_search_path(Path::new(program));
    let mut command = plain_command(program);
    command.envs(shell_environment());
    if let Some(search_path) = search_path {
        command.env("PATH", search_path);
    }
    command
}

/// The `PATH` a provider CLI runs with: every directory Waku itself searched,
/// plus the one the binary was found in.
///
/// Detection resolves CLIs from more directories than the desktop process
/// inherits — a Bun or npm global prefix that the GUI `PATH` predates, for
/// example — so a CLI found in one of them has to *run* with them too.
/// Launcher-based installs depend on it and fail silently without it: Bun's
/// Windows `pi.EXE` is a shim that launches `bun.exe` from its own directory,
/// and both an npm `.cmd` shim and a `/usr/bin/env node` shebang need `node`
/// on the child's `PATH`. Detection still succeeds in that state — it only
/// looks for the file — so the provider shows up as installed while every
/// probe it runs comes back empty.
///
/// Windows needs this most: the login-shell probe there is best-effort — no
/// PowerShell may be present, and a profile can refuse to load — so a
/// GUI-launched Waku can still be running with only the `PATH` it inherited.
fn child_search_path(program: &Path) -> Option<OsString> {
    let mut directories = executable_search_paths();
    // Last, not first: an install outside the known prefixes still finds its
    // runtime, while the user's own `PATH` order decides everything else.
    directories.extend(
        program
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .map(Path::to_path_buf),
    );
    let mut seen = HashSet::new();
    directories.retain(|directory| seen.insert(directory.clone()));
    std::env::join_paths(directories).ok()
}

/// A command that never flashes a console window.
///
/// Waku's Windows build is a GUI-subsystem binary with no console of its own,
/// so `CreateProcess` allocates one for every console child — `git`, a
/// provider CLI, the daemon — and flashes it on screen. `CREATE_NO_WINDOW`
/// keeps the child's console hidden while its pipes still work.
pub fn plain_command(program: impl AsRef<OsStr>) -> Command {
    let mut command = Command::new(program);
    detach_console(&mut command);
    command
}

fn detach_console(command: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;

        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    let _ = command;
}

/// Spawn `command` with `SIGCHLD` unblocked in the child. On macOS, libdispatch
/// worker threads (which back GPUI's background executor) block `SIGCHLD`, and
/// a process spawned from such a thread inherits the blocked mask. That breaks
/// provider-side async process reapers. The caller's mask is restored as soon
/// as the child has been created.
pub fn spawn(command: &mut Command) -> io::Result<Child> {
    detach_console(command);
    with_sigchld_unblocked(|| command.spawn())
}

/// Spawn `command` through [`spawn`] and collect its output.
///
/// `Command::spawn` inherits standard streams by default, unlike
/// `Command::output`. Own all three streams here so callers keep the latter's
/// behavior while the signal mask is changed only for the spawn itself.
pub fn output(command: &mut Command) -> io::Result<Output> {
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
    if candidate.components().count() > 1 {
        return resolve_executable_file(candidate);
    }
    executable_search_paths()
        .into_iter()
        .find_map(|directory| resolve_executable_file(&directory.join(name)))
}

/// On Windows, try the path with each `PATHEXT` suffix first, then accept
/// `candidate` as it stands. Elsewhere the candidate is used directly.
///
/// Nothing is executable by name alone on Windows: an npm-installed provider
/// CLI lands as `claude.cmd` beside `claude.ps1`, and Bun and Cargo install
/// `.exe`. Trying `PATHEXT` in its configured order picks the same file the
/// shell would, and `std::process::Command` runs a `.cmd`/`.bat` through
/// `cmd.exe` for us.
///
/// A global npm install also writes an extensionless POSIX shim next to those
/// two, and `CreateProcess` cannot run it. Accepting the bare name before the
/// suffixes would hand back that shim and every launch of the provider would
/// fail, so the suffixed names have to win.
fn resolve_executable_file(candidate: &Path) -> Option<PathBuf> {
    #[cfg(windows)]
    if let Some(stem) = candidate.file_name() {
        let stem = stem.to_owned();
        for extension in executable_extensions() {
            let mut name = stem.clone();
            name.push(&extension);
            let suffixed = candidate.with_file_name(name);
            if suffixed.is_file() {
                return Some(suffixed);
            }
        }
    }
    if candidate.is_file() {
        return Some(candidate.to_path_buf());
    }
    None
}

#[cfg(windows)]
fn executable_extensions() -> Vec<OsString> {
    const DEFAULT_PATHEXT: &str = ".COM;.EXE;.BAT;.CMD";

    let configured = std::env::var("PATHEXT").unwrap_or_default();
    let configured = if configured.trim().is_empty() {
        DEFAULT_PATHEXT
    } else {
        configured.as_str()
    };
    configured
        .split(';')
        .map(str::trim)
        .filter(|extension| extension.starts_with('.'))
        .map(OsString::from)
        .collect()
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
#[cfg(unix)]
pub fn refresh_from_default_shell() -> bool {
    let Some(environment) = resolve_default_shell_environment(LOGIN_SHELL_ENV_TIMEOUT) else {
        return false;
    };
    *login_shell_environment()
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(environment);
    true
}

/// Windows has no login shell, but it still needs this probe. A GUI-launched
/// Waku inherits explorer's `PATH`, which predates later installs, and the
/// package managers users actually add to it — fnm, Volta, nvm — extend
/// `PATH` only in the PowerShell profile, which never reaches the machine or
/// user environment block. Probe PowerShell with the profile loaded, capture
/// the fresh user and machine registry `PATH` values in the same run, and
/// cache the merge for provider discovery and every later child. This starts
/// PowerShell — which runs the user's profile — and must therefore only be
/// called from a background thread.
#[cfg(windows)]
pub fn refresh_from_default_shell() -> bool {
    let Some(environment) = resolve_windows_profile_environment(LOGIN_SHELL_ENV_TIMEOUT) else {
        return false;
    };
    *login_shell_environment()
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(environment);
    true
}

/// Targets with neither probe keep the inherited environment.
#[cfg(not(any(unix, windows)))]
pub fn refresh_from_default_shell() -> bool {
    true
}

/// A hanging or exiting profile loses the whole probe run, so the
/// no-profile retry gets its own short leash.
#[cfg(windows)]
const WINDOWS_NO_PROFILE_ENV_TIMEOUT: Duration = Duration::from_secs(2);

/// The PowerShell script the probe runs. Each captured variable is written to
/// the capture file as NUL-separated `name=value` entries — the same format
/// as `env -0` on Unix — so profile noise on stdout cannot corrupt the
/// result. `[Environment]::GetEnvironmentVariable` reads the child's
/// *process* environment, which includes whatever the profile did to
/// `$env:PATH`; the two-argument form reads the fresh registry blocks, which
/// the inherited `PATH` may be older than.
#[cfg(windows)]
const WINDOWS_ENV_CAPTURE_COMMAND: &str = "\
$ErrorActionPreference = 'Continue'
$entries = New-Object System.Collections.Generic.List[string]
foreach ($name in @('PATH', 'FNM_DIR', 'FNM_MULTISHELL_PATH')) {
  $value = [Environment]::GetEnvironmentVariable($name)
  if ($value) { $entries.Add($name + '=' + $value) }
}
foreach ($target in @('User', 'Machine')) {
  $value = [Environment]::GetEnvironmentVariable('PATH', $target)
  if ($value) { $entries.Add('WAKU_' + $target.ToUpper() + '_PATH=' + [Environment]::ExpandEnvironmentVariables($value)) }
}
[IO.File]::WriteAllText($env:WAKU_SHELL_ENV_CAPTURE_FILE, [string]::Join([string][char]0, $entries))
";

/// PowerShell 7 first, then the in-box Windows PowerShell. `cmd.exe` is not a
/// candidate: it has no profile to load and no registry API.
#[cfg(windows)]
fn windows_powershell_candidates() -> Vec<PathBuf> {
    ["pwsh.exe", "powershell.exe"]
        .into_iter()
        .filter_map(find_executable)
        .collect()
}

#[cfg(windows)]
fn resolve_windows_profile_environment(timeout: Duration) -> Option<ShellEnvironment> {
    let started_at = Instant::now();
    // Load each shell's profile first — fnm/Volta/nvm extend PATH there and
    // nowhere else. The registry PATH is captured by the same run, so a
    // profile that merely lacks PATH still succeeds via the registry values.
    for shell in windows_powershell_candidates() {
        let remaining = timeout.checked_sub(started_at.elapsed())?;
        if remaining.is_zero() {
            return None;
        }
        if let Some(environment) = capture_windows_environment(&shell, true, remaining) {
            if let Some(environment) = merge_windows_environment(environment) {
                return Some(environment);
            }
        }
    }
    // A hanging or exiting profile loses the whole run. Retry without it on a
    // short leash; the registry PATH alone is still worth the spawn.
    for shell in windows_powershell_candidates() {
        if let Some(environment) =
            capture_windows_environment(&shell, false, WINDOWS_NO_PROFILE_ENV_TIMEOUT)
        {
            if let Some(environment) = merge_windows_environment(environment) {
                return Some(environment);
            }
        }
    }
    None
}

#[cfg(windows)]
fn capture_windows_environment(
    shell: &Path,
    load_profile: bool,
    timeout: Duration,
) -> Option<ShellEnvironment> {
    let capture = ShellEnvironmentCapture::create()?;
    let mut command = Command::new(shell);
    command.arg("-NoLogo").arg("-NonInteractive");
    if !load_profile {
        command.arg("-NoProfile");
    }
    command
        .arg("-Command")
        .arg(WINDOWS_ENV_CAPTURE_COMMAND)
        .env("WAKU_SHELL_ENV_CAPTURE_FILE", capture.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = spawn(&mut command).ok()?;
    if !wait_for_child(&mut child, timeout) {
        return None;
    }
    parse_shell_environment(&fs::read(capture.path()).ok()?)
}

/// Combine the profile variables with the fresh registry `PATH` values into
/// the cached environment. The profile `PATH` comes first — it is the user's
/// own order — then the user registry `PATH`, then the machine one; the
/// inherited `PATH` is appended afterwards by [`search_paths_from`], and a
/// child `PATH` built by [`child_search_path`] keeps that order. fnm's
/// variables ride along so its shims can resolve their Node installation.
/// `PATH` matching is case-insensitive on Windows, so deduplicate that way.
#[cfg(windows)]
fn merge_windows_environment(mut environment: ShellEnvironment) -> Option<ShellEnvironment> {
    let mut directories = Vec::new();
    for name in ["PATH", "WAKU_USER_PATH", "WAKU_MACHINE_PATH"] {
        if let Some(value) = take_environment_variable(&mut environment, name) {
            directories.extend(std::env::split_paths(&value));
        }
    }
    let mut seen = HashSet::new();
    directories
        .retain(|directory| seen.insert(directory.as_os_str().to_string_lossy().to_lowercase()));
    if directories.is_empty() {
        return None;
    }
    let path = std::env::join_paths(directories).ok()?;
    environment.insert(0, (OsString::from("PATH"), path));
    Some(environment)
}

#[cfg(windows)]
fn take_environment_variable(environment: &mut ShellEnvironment, name: &str) -> Option<OsString> {
    let position = environment
        .iter()
        .position(|(candidate, _)| candidate.to_string_lossy().eq_ignore_ascii_case(name))?;
    Some(environment.remove(position).1)
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
        directories.extend(user_tool_directories(home));
    }
    directories.extend(system_tool_directories());

    let mut seen = HashSet::new();
    directories.retain(|directory| seen.insert(directory.clone()));
    directories
}

/// Where per-user package managers put the provider CLIs, in case the
/// inherited `PATH` predates the install.
#[cfg(not(windows))]
fn user_tool_directories(home: &Path) -> Vec<PathBuf> {
    vec![
        home.join(".local/bin"),
        home.join(".bun/bin"),
        home.join(".cargo/bin"),
        home.join(".local/share/mise/shims"),
        home.join(".volta/bin"),
    ]
}

#[cfg(windows)]
fn user_tool_directories(home: &Path) -> Vec<PathBuf> {
    let mut directories = vec![
        // npm's global prefix, where a `claude.cmd` shim lands.
        home.join("AppData/Roaming/npm"),
        home.join(".bun/bin"),
        home.join(".cargo/bin"),
        home.join("scoop/shims"),
        home.join("AppData/Local/Microsoft/WindowsApps"),
        home.join(".local/bin"),
    ];
    // Volta, pnpm, and the user-scoped Node installer default to LocalAppData
    // (the same list T3 Code probes); it can be redirected away from home.
    if let Some(local_app_data) = std::env::var_os("LOCALAPPDATA") {
        let local_app_data = PathBuf::from(local_app_data);
        directories.push(local_app_data.join("Volta/bin"));
        directories.push(local_app_data.join("pnpm"));
        directories.push(local_app_data.join("Programs/nodejs"));
    }
    directories
}

#[cfg(not(windows))]
fn system_tool_directories() -> Vec<PathBuf> {
    vec![
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
        PathBuf::from("/usr/bin"),
        PathBuf::from("/bin"),
        PathBuf::from("/usr/sbin"),
        PathBuf::from("/sbin"),
    ]
}

#[cfg(windows)]
fn system_tool_directories() -> Vec<PathBuf> {
    let mut directories = Vec::new();
    if let Some(program_files) = std::env::var_os("ProgramFiles") {
        directories.push(PathBuf::from(program_files).join("nodejs"));
    }
    if let Some(system_root) = std::env::var_os("SystemRoot") {
        let system32 = PathBuf::from(system_root).join("System32");
        directories.push(system32.join("WindowsPowerShell/v1.0"));
        directories.push(system32);
    }
    directories
}

#[cfg(unix)]
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
    // `SHELL` is a POSIX convention. On Windows it is set only by ported
    // toolchains such as Git Bash, and usually to an MSYS path that Win32
    // cannot open, so the native shells are resolved instead.
    #[cfg(unix)]
    {
        if let Some(shell) = std::env::var_os("SHELL").filter(|shell| !shell.is_empty()) {
            candidates.push(PathBuf::from(shell));
        }
        if let Some(shell) = account_default_shell() {
            candidates.push(shell);
        }
    }
    #[cfg(target_os = "macos")]
    candidates.push(PathBuf::from("/bin/zsh"));
    #[cfg(target_os = "linux")]
    candidates.extend([PathBuf::from("/bin/bash"), PathBuf::from("/bin/sh")]);
    #[cfg(windows)]
    candidates.extend(windows_shell_candidates());

    let mut seen = HashSet::new();
    candidates.retain(|shell| seen.insert(shell.clone()));
    candidates
}

/// PowerShell 7 first, then the in-box Windows PowerShell, then whatever
/// `COMSPEC` names — the same order a Windows Terminal profile list uses.
#[cfg(windows)]
fn windows_shell_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    for shell in ["pwsh.exe", "powershell.exe"] {
        candidates.extend(find_executable(shell));
    }
    if let Some(comspec) = std::env::var_os("COMSPEC").filter(|comspec| !comspec.is_empty()) {
        candidates.push(PathBuf::from(comspec));
    }
    candidates
}

/// Pick the user's configured login shell for an interactive terminal, with a
/// platform shell as a final fallback when desktop launchers omit `SHELL`.
pub fn default_terminal_shell() -> PathBuf {
    let mut candidates = Vec::new();
    // The shell the user chose outranks whatever `SHELL` was inherited from;
    // see the note in waku-client's `unix_terminal_shell_candidates`. The
    // environment probe below keeps its own order, since it wants the shell
    // whose rc files produced this process's `PATH`.
    #[cfg(unix)]
    candidates.extend(account_default_shell());
    candidates.extend(default_shell_candidates());

    candidates
        .into_iter()
        .find(|shell| shell.is_file())
        .unwrap_or_else(default_terminal_shell_fallback)
}

#[cfg(not(windows))]
fn default_terminal_shell_fallback() -> PathBuf {
    PathBuf::from("/bin/sh")
}

#[cfg(windows)]
fn default_terminal_shell_fallback() -> PathBuf {
    PathBuf::from("cmd.exe")
}

/// The arguments that open `shell` the way the user's own terminal would.
///
/// A POSIX shell needs `-l` so the login files that set `PATH` are read.
/// Windows applies the environment before the process starts, so there is no
/// login mode to ask for — PowerShell's `-Login` exists on Unix hosts only,
/// and passing it here is an error. Suppressing its banner is the one thing
/// worth saying.
pub fn default_terminal_shell_args(shell: &Path) -> Vec<String> {
    #[cfg(not(windows))]
    {
        let _ = shell;
        vec!["-l".to_owned()]
    }
    #[cfg(windows)]
    {
        let is_powershell = shell
            .file_stem()
            .and_then(|stem| stem.to_str())
            .is_some_and(|stem| {
                stem.eq_ignore_ascii_case("pwsh") || stem.eq_ignore_ascii_case("powershell")
            });
        if is_powershell {
            vec!["-NoLogo".to_owned()]
        } else {
            Vec::new()
        }
    }
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

#[cfg(unix)]
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

fn os_string_from_bytes(bytes: &[u8]) -> Option<OsString> {
    #[cfg(unix)]
    {
        Some(OsString::from_vec(bytes.to_vec()))
    }
    #[cfg(windows)]
    {
        // The Windows probe writes UTF-8; PowerShell encodes .NET strings
        // exactly, so lossy decoding loses nothing.
        Some(String::from_utf8_lossy(bytes).into_owned().into())
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = bytes;
        None
    }
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

    fn command_search_path(command: &Command) -> Vec<PathBuf> {
        let path = command
            .get_envs()
            .find(|(key, _)| *key == OsStr::new("PATH"))
            .and_then(|(_, value)| value)
            .expect("a provider command sets PATH for its child");
        std::env::split_paths(path).collect()
    }

    /// A CLI resolved from a directory the desktop `PATH` never had must run
    /// with that directory too, or its own launcher — a Bun shim, an npm
    /// `.cmd`, an `env node` shebang — cannot find its runtime.
    #[test]
    fn a_provider_cli_runs_with_the_directories_detection_searched() {
        #[cfg(windows)]
        let program = PathBuf::from("C:\\waku-fixture\\bin\\pi.exe");
        #[cfg(not(windows))]
        let program = PathBuf::from("/opt/waku-fixture/bin/pi");

        let directories = command_search_path(&command(&program));

        assert!(directories.contains(&program.parent().expect("fixture parent").to_path_buf()));
        for searched in executable_search_paths() {
            assert!(
                directories.contains(&searched),
                "{} is searched during detection but missing from the child PATH",
                searched.display()
            );
        }
    }

    #[test]
    fn a_bare_program_name_contributes_no_search_directory() {
        let directories = command_search_path(&command("git"));

        assert_eq!(directories, executable_search_paths());
    }

    #[cfg(unix)]
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

    #[cfg(unix)]
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

    #[cfg(unix)]
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

    #[cfg(windows)]
    #[test]
    fn desktop_path_is_extended_with_the_windows_package_manager_prefixes() {
        let home = Path::new("C:\\Users\\example");
        let paths = search_paths_from(
            None,
            Some(OsStr::new("C:\\Windows\\System32;C:\\Windows")),
            Some(home),
        );

        assert_eq!(paths[0], PathBuf::from("C:\\Windows\\System32"));
        assert_eq!(paths[1], PathBuf::from("C:\\Windows"));
        assert!(paths.contains(&home.join("AppData/Roaming/npm")));
        assert!(paths.contains(&home.join(".bun/bin")));
        assert!(paths.contains(&home.join("scoop/shims")));
        assert!(paths.contains(&home.join(".local/bin")));
        if let Some(local_app_data) = std::env::var_os("LOCALAPPDATA") {
            let local_app_data = PathBuf::from(local_app_data);
            assert!(paths.contains(&local_app_data.join("Volta/bin")));
            assert!(paths.contains(&local_app_data.join("pnpm")));
            assert!(paths.contains(&local_app_data.join("Programs/nodejs")));
        }
        assert_eq!(
            paths
                .iter()
                .filter(|path| *path == Path::new("C:\\Windows"))
                .count(),
            1
        );
    }

    /// The probe script must execute as written against the in-box Windows
    /// PowerShell: `-NoProfile` leaves the child `PATH` untouched, so the
    /// captured value is exactly the one this process inherited.
    #[cfg(windows)]
    #[test]
    fn windows_environment_probe_captures_the_inherited_path_without_a_profile() {
        let capture = ShellEnvironmentCapture::create().expect("create capture file");
        let mut command = Command::new("powershell.exe");
        command
            .args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command"])
            .arg(WINDOWS_ENV_CAPTURE_COMMAND)
            .env("WAKU_SHELL_ENV_CAPTURE_FILE", capture.path())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut child = spawn(&mut command).expect("spawn PowerShell probe");
        assert!(
            wait_for_child(&mut child, Duration::from_secs(10)),
            "PowerShell probe did not finish in time"
        );
        let environment =
            parse_shell_environment(&fs::read(capture.path()).expect("capture file written"))
                .expect("parse captured environment");
        let path = environment
            .iter()
            .find(|(name, _)| name == OsStr::new("PATH"))
            .map(|(_, value)| value.clone())
            .expect("captured PATH");
        assert_eq!(path, std::env::var_os("PATH").expect("inherited PATH"));
    }

    #[cfg(windows)]
    #[test]
    fn windows_profile_environment_merges_registry_paths_behind_the_profile_path() {
        let environment = merge_windows_environment(vec![
            (OsString::from("FNM_DIR"), OsString::from("C:\\fnm")),
            (
                OsString::from("PATH"),
                OsString::from("C:\\profile-first;C:\\shared"),
            ),
            (
                OsString::from("WAKU_USER_PATH"),
                OsString::from("C:\\user;C:\\shared"),
            ),
            (
                OsString::from("WAKU_MACHINE_PATH"),
                OsString::from("C:\\machine;C:\\USER;C:\\SHARED"),
            ),
        ])
        .expect("merge captured environment");

        let path = environment
            .iter()
            .find(|(name, _)| name == OsStr::new("PATH"))
            .map(|(_, value)| value.clone())
            .expect("merged PATH");
        assert_eq!(
            std::env::split_paths(&path).collect::<Vec<_>>(),
            vec![
                PathBuf::from("C:\\profile-first"),
                PathBuf::from("C:\\shared"),
                PathBuf::from("C:\\user"),
                PathBuf::from("C:\\machine"),
            ]
        );
        assert!(environment.contains(&(OsString::from("FNM_DIR"), OsString::from("C:\\fnm"))));
        assert!(
            !environment
                .iter()
                .any(|(name, _)| name == OsStr::new("WAKU_USER_PATH"))
        );
        assert!(
            !environment
                .iter()
                .any(|(name, _)| name == OsStr::new("WAKU_MACHINE_PATH"))
        );
    }

    #[cfg(windows)]
    #[test]
    fn a_bare_name_resolves_through_pathext() {
        let directory = std::env::temp_dir().join(format!("waku-pathext-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).expect("create fixture directory");
        // Only a suffixed file exists here, so the bare name resolves through PATHEXT.
        // A global npm install also drops an extensionless shim beside it; that layout is
        // covered by a_bare_name_prefers_pathext_over_an_extensionless_shim.
        std::fs::write(directory.join("faux-provider.cmd"), "@echo off\n")
            .expect("write shim fixture");

        assert_eq!(
            resolve_executable_file(&directory.join("faux-provider"))
                .expect("resolve through PATHEXT")
                .to_string_lossy()
                .to_lowercase(),
            directory
                .join("faux-provider.cmd")
                .to_string_lossy()
                .to_lowercase(),
        );
        assert_eq!(resolve_executable_file(&directory.join("absent")), None);

        let _ = std::fs::remove_dir_all(&directory);
    }

    #[cfg(windows)]
    #[test]
    fn a_bare_name_prefers_pathext_over_an_extensionless_shim() {
        // A global npm install writes all three of these side by side. Only the `.cmd`
        // can be launched by `CreateProcess`; the extensionless one is a POSIX shim.
        let directory =
            std::env::temp_dir().join(format!("waku-pathext-shim-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).expect("create fixture directory");
        std::fs::write(directory.join("faux-provider"), "#!/bin/sh\n")
            .expect("write posix shim fixture");
        std::fs::write(directory.join("faux-provider.ps1"), "#!/usr/bin/env pwsh\n")
            .expect("write powershell shim fixture");
        std::fs::write(directory.join("faux-provider.cmd"), "@echo off\n")
            .expect("write cmd shim fixture");

        assert_eq!(
            resolve_executable_file(&directory.join("faux-provider"))
                .expect("resolve through PATHEXT")
                .to_string_lossy()
                .to_lowercase(),
            directory
                .join("faux-provider.cmd")
                .to_string_lossy()
                .to_lowercase(),
        );

        let _ = std::fs::remove_dir_all(&directory);
    }

    #[cfg(windows)]
    #[test]
    fn windows_terminal_shells_never_ask_for_a_login_session() {
        assert_eq!(
            default_terminal_shell_args(Path::new("C:\\Program Files\\PowerShell\\7\\pwsh.exe")),
            vec!["-NoLogo".to_owned()]
        );
        assert!(
            default_terminal_shell_args(Path::new("C:\\Windows\\System32\\cmd.exe")).is_empty()
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

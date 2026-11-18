//! Client-host shell selection for the desktop's local terminal surface.

use std::collections::HashSet;
use std::ffi::{OsStr, OsString};

use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::ffi::CStr;
#[cfg(unix)]
use std::mem::MaybeUninit;
#[cfg(unix)]
use std::os::unix::ffi::OsStringExt;

use serde::{Deserialize, Serialize};

/// The environment variable that names the shell for Waku's terminals.
///
/// It is the escape hatch for the people who cannot edit `app.json`: the
/// value is a path, or a name to look up on `PATH`.
pub const TERMINAL_SHELL_ENV: &str = "WAKU_TERMINAL_SHELL";

/// The user's terminal preferences, as stored in `app.json`.
///
/// The account's login shell is what a terminal opened inside Waku used to
/// run, and it is still the default. A login shell and the shell someone
/// actually wants to type in are often two different programs — a POSIX
/// login shell with fish or nushell in the terminals is a common setup — and
/// only an explicit choice can bridge that.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(default)]
pub struct TerminalSettings {
    /// The shell the terminal pane runs. `None` keeps the account's login
    /// shell.
    pub shell: Option<String>,
    /// The arguments that shell is started with. Empty keeps the platform
    /// defaults for the shell that was resolved.
    pub args: Vec<String>,
}

/// The shell a terminal runs, and the arguments it runs with.
#[derive(Clone, Debug, PartialEq)]
pub struct TerminalShell {
    pub program: PathBuf,
    pub args: Vec<String>,
}

pub fn executable_search_path() -> Option<OsString> {
    let mut directories = std::env::var_os("PATH")
        .as_deref()
        .map(std::env::split_paths)
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    if let Some(home) = dirs::home_dir() {
        directories.extend(user_tool_directories(&home));
    }
    directories.extend(system_tool_directories());
    let mut seen = HashSet::new();
    directories.retain(|directory| seen.insert(directory.clone()));
    std::env::join_paths(directories).ok()
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
    vec![
        // npm's global prefix, where a `claude.cmd` shim lands.
        home.join("AppData/Roaming/npm"),
        home.join(".bun/bin"),
        home.join(".cargo/bin"),
        home.join("scoop/shims"),
        home.join("AppData/Local/Microsoft/WindowsApps"),
    ]
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

/// The shell a terminal runs when nothing is configured.
///
/// `WAKU_TERMINAL_SHELL` still applies: it is the same choice, made where
/// the settings file cannot be edited.
pub fn default_terminal_shell() -> PathBuf {
    first_existing_shell(terminal_shell_candidates(
        None,
        shell_from_environment(),
        account_default_shell(),
        std::env::var_os("SHELL").as_deref(),
    ))
}

/// The shell a terminal runs, given the user's preferences.
///
/// The order is the configured shell first, then `WAKU_TERMINAL_SHELL`, then
/// the account's login shell, then `SHELL`, then a platform shell. A
/// configured shell that cannot be found is skipped rather than fatal, so a
/// path that no longer exists falls back to the shell the user actually has.
pub fn terminal_shell(settings: &TerminalSettings) -> TerminalShell {
    let configured = settings.shell.as_deref().and_then(resolve_shell);
    let candidates = terminal_shell_candidates(
        configured,
        shell_from_environment(),
        account_default_shell(),
        std::env::var_os("SHELL").as_deref(),
    );
    let program = first_existing_shell(candidates);
    let args = if settings.args.is_empty() {
        default_terminal_shell_args(&program)
    } else {
        settings.args.clone()
    };
    TerminalShell { program, args }
}

/// The first candidate that is a file, or a platform shell when none is.
fn first_existing_shell(candidates: Vec<PathBuf>) -> PathBuf {
    candidates
        .into_iter()
        .find(|shell| shell.is_file())
        .unwrap_or_else(default_terminal_shell_fallback)
}

/// The shell named by `WAKU_TERMINAL_SHELL`.
///
/// The value is a path, or a name to look up on `PATH`.
fn shell_from_environment() -> Option<PathBuf> {
    let value = std::env::var_os(TERMINAL_SHELL_ENV)?;
    resolve_shell(&value.to_string_lossy())
}

/// Find the executable a configured shell names.
///
/// A path is taken as it was written — the user pointed at a file — and a
/// bare name is looked up on `PATH`, the way a shell would find it.
pub fn resolve_shell(configured: &str) -> Option<PathBuf> {
    let trimmed = configured.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.contains('/') || trimmed.contains('\\') {
        let path = Path::new(trimmed);
        return path.is_file().then(|| path.to_path_buf());
    }
    let search_path = executable_search_path()?;
    let names = shell_file_names(trimmed);
    for directory in std::env::split_paths(&search_path) {
        for name in &names {
            let candidate = directory.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// The file names a bare shell name can have on this platform.
#[cfg(windows)]
fn shell_file_names(name: &str) -> Vec<String> {
    let mut names = vec![name.to_owned()];
    for extension in ["exe", "cmd", "bat"] {
        let candidate = format!("{name}.{extension}");
        if !names.contains(&candidate) {
            names.push(candidate);
        }
    }
    names
}

#[cfg(not(windows))]
fn shell_file_names(name: &str) -> Vec<String> {
    vec![name.to_owned()]
}

/// The shells a terminal may run, in the order they are tried.
///
/// A configured shell comes first — the user picked it on purpose — then the
/// environment override, then the account's login shell, then `SHELL`, then
/// a platform shell.
fn terminal_shell_candidates(
    configured: Option<PathBuf>,
    environment_override: Option<PathBuf>,
    account_shell: Option<PathBuf>,
    shell_environment: Option<&OsStr>,
) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    candidates.extend(configured);
    candidates.extend(environment_override);
    #[cfg(unix)]
    {
        // The account's login shell first, then `SHELL`.
        //
        // `SHELL` describes whatever launched Waku — a desktop session, the
        // dev watcher, a terminal profile pinned to a different shell — and on
        // macOS it keeps the value the login session started with, so it goes
        // stale the moment someone runs `chsh`. The passwd entry is the shell
        // the user actually chose, which is what a terminal opened inside the
        // app should be. `SHELL` still wins over the platform fallback, so an
        // explicitly exported one is honored when the passwd entry is missing
        // or points at something unusable.
        candidates.extend(account_shell);
        candidates.extend(
            shell_environment
                .filter(|shell| !shell.is_empty())
                .map(PathBuf::from),
        );
        #[cfg(target_os = "macos")]
        candidates.push(PathBuf::from("/bin/zsh"));
        #[cfg(target_os = "linux")]
        candidates.extend([PathBuf::from("/bin/bash"), PathBuf::from("/bin/sh")]);
    }
    #[cfg(windows)]
    {
        // `SHELL` is a POSIX convention. On Windows it is set only by ported
        // toolchains such as Git Bash, and usually to an MSYS path that Win32
        // cannot open, so the native shells are resolved instead.
        let _ = (account_shell, shell_environment);
        candidates.extend(windows_shell_candidates());
    }
    candidates
}

/// PowerShell 7 first, then the in-box Windows PowerShell, then whatever
/// `COMSPEC` names — the same order a Windows Terminal profile list uses.
#[cfg(windows)]
fn windows_shell_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    let search_path = executable_search_path();
    let directories = search_path
        .as_deref()
        .map(std::env::split_paths)
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    for shell in ["pwsh.exe", "powershell.exe"] {
        candidates.extend(
            directories
                .iter()
                .map(|directory| directory.join(shell))
                .find(|candidate| candidate.is_file()),
        );
    }
    candidates.extend(
        std::env::var_os("COMSPEC")
            .filter(|comspec| !comspec.is_empty())
            .map(PathBuf::from),
    );
    candidates
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
///
/// A shell that is neither takes nothing: nushell has no `-l`, and a
/// terminal that refuses to start is worse than one that starts plain.
pub fn default_terminal_shell_args(shell: &Path) -> Vec<String> {
    let stem = shell
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match stem.as_str() {
        "pwsh" | "powershell" => vec!["-NoLogo".to_owned()],
        // Every POSIX shell spells its login mode the same way.
        _ if stem.ends_with("sh") => vec!["-l".to_owned()],
        _ => Vec::new(),
    }
}

/// The account's login shell, from the passwd entry.
///
/// Windows has no passwd entry to read, so there is no login shell there.
#[cfg(windows)]
fn account_default_shell() -> Option<PathBuf> {
    None
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn the_accounts_login_shell_outranks_an_inherited_shell_variable() {
        let candidates = terminal_shell_candidates(
            None,
            None,
            Some(PathBuf::from("/opt/homebrew/bin/fish")),
            Some(OsStr::new("/bin/zsh")),
        );

        assert_eq!(candidates[0], PathBuf::from("/opt/homebrew/bin/fish"));
        assert_eq!(candidates[1], PathBuf::from("/bin/zsh"));
    }

    #[test]
    fn an_exported_shell_is_still_used_when_the_account_has_none() {
        let candidates = terminal_shell_candidates(None, None, None, Some(OsStr::new("/bin/bash")));

        assert_eq!(candidates[0], PathBuf::from("/bin/bash"));
    }

    #[test]
    fn an_empty_shell_variable_is_ignored_and_a_platform_shell_remains() {
        let candidates = terminal_shell_candidates(None, None, None, Some(OsStr::new("")));

        assert!(!candidates.is_empty());
        assert!(!candidates.contains(&PathBuf::new()));
    }

    #[test]
    fn a_configured_shell_outranks_the_accounts_login_shell() {
        let candidates = terminal_shell_candidates(
            Some(PathBuf::from("/opt/homebrew/bin/fish")),
            Some(PathBuf::from("/usr/bin/nu")),
            Some(PathBuf::from("/bin/bash")),
            Some(OsStr::new("/bin/zsh")),
        );

        assert_eq!(candidates[0], PathBuf::from("/opt/homebrew/bin/fish"));
        assert_eq!(candidates[1], PathBuf::from("/usr/bin/nu"));
        assert_eq!(candidates[2], PathBuf::from("/bin/bash"));
    }

    #[test]
    fn a_posix_shell_starts_as_a_login_shell_and_a_nushell_does_not() {
        assert_eq!(
            default_terminal_shell_args(Path::new("/bin/bash")),
            vec!["-l".to_owned()]
        );
        assert_eq!(
            default_terminal_shell_args(Path::new("/usr/bin/nu")),
            Vec::<String>::new()
        );
    }
}

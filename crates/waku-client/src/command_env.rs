//! Client-host shell selection for the desktop's local terminal surface.

use std::collections::HashSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::ffi::CStr;
#[cfg(unix)]
use std::mem::MaybeUninit;
#[cfg(unix)]
use std::os::unix::ffi::OsStringExt;

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

pub fn default_terminal_shell() -> PathBuf {
    let mut candidates = Vec::new();
    // `SHELL` is a POSIX convention. On Windows it is set only by ported
    // toolchains such as Git Bash, and usually to an MSYS path that Win32
    // cannot open, so the native shells are resolved instead.
    #[cfg(unix)]
    {
        candidates.extend(
            std::env::var_os("SHELL")
                .filter(|shell| !shell.is_empty())
                .map(PathBuf::from),
        );
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

    candidates
        .into_iter()
        .find(|shell| shell.is_file())
        .unwrap_or_else(default_terminal_shell_fallback)
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

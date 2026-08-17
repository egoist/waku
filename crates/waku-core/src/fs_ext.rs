//! Cross-platform stand-ins for the POSIX filesystem calls Waku relies on.
//!
//! Provider isolation directories, the Computer Use install root, and the
//! bundle copies that feed it are all written in POSIX terms. Windows has no
//! mode bits and reaches symlinks through two separate calls, so the callers
//! go through these helpers instead of `std::os::unix` directly.

use std::io;
use std::path::Path;

/// Create `path` and any missing parents so only the current user can reach
/// it.
///
/// Windows has no mode bits: the locations Waku creates here live under the
/// user's own profile (`%LOCALAPPDATA%`, `%TEMP%`), which already inherits an
/// ACL granting the owner and administrators alone.
pub(crate) fn create_private_dir_all(path: &Path) -> io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        builder.mode(0o700);
    }
    builder.create(path)
}

/// Drop group and other access on an existing directory. A no-op on Windows,
/// where the inherited profile ACL already restricts it.
pub(crate) fn restrict_to_owner(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

/// Link `link` to `original`.
///
/// Windows picks the symlink flavor from the target's kind, and creating
/// either needs Developer Mode or `SeCreateSymbolicLinkPrivilege`; callers
/// treat a failure as "this resource could not be mirrored" rather than
/// assuming POSIX semantics.
pub(crate) fn symlink(original: &Path, link: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(original, link)
    }
    #[cfg(windows)]
    {
        if original.is_dir() {
            std::os::windows::fs::symlink_dir(original, link)
        } else {
            std::os::windows::fs::symlink_file(original, link)
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (original, link);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "symlinks are not supported on this platform",
        ))
    }
}

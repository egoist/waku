//! Private workspaces for tasks that are not attached to a user project.
//!
//! Codex allocates ordinary projectless chats beneath a per-user root using
//! `<root>/<local date>/<prompt slug>`, with numeric collision suffixes and a
//! random fallback. Waku mirrors that layout beneath `~/.waku/projects` so
//! generated workspaces do not sit beside configuration documents.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};

use chrono::{Local, NaiveDate};
use uuid::Uuid;

const DEFAULT_SLUG: &str = "new-chat";
const MAX_SLUG_BYTES: usize = 80;
const MAX_NUMBERED_CANDIDATES: usize = 100;
const MAX_RANDOM_CANDIDATES: usize = 5;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Workspace {
    pub cwd: PathBuf,
    pub workspace_root: PathBuf,
}

/// The daemon root is cached because `Project::is_projectless` is reached from
/// row builders and render paths. A remote client installs the daemon-host
/// value during task-state loading; callers then perform path compares only.
fn workspace_root_slot() -> &'static RwLock<Option<PathBuf>> {
    static ROOT: OnceLock<RwLock<Option<PathBuf>>> = OnceLock::new();
    ROOT.get_or_init(|| {
        RwLock::new(dirs::home_dir().map(|home| home.join(".waku").join("projects")))
    })
}

pub fn set_workspace_root(root: Option<PathBuf>) {
    waku_protocol::projectless::set_workspace_root(root.clone());
    if let Ok(mut current) = workspace_root_slot().write() {
        *current = root;
    }
}

pub fn workspace_root() -> Option<PathBuf> {
    workspace_root_slot().read().ok()?.clone()
}

/// Home directory on the host that owns the configured projectless root.
/// Remote desktops use this only to abbreviate daemon paths for display.
pub fn home_directory() -> Option<PathBuf> {
    let root = workspace_root()?;
    root.parent()?.parent().map(Path::to_path_buf)
}

/// Existing builds created dated workspaces directly under `~/.waku`; keep
/// recognizing those paths while all new workspaces live under `projects/`.
pub fn is_projectless_path(path: &Path) -> bool {
    workspace_root().is_some_and(|root| {
        path.starts_with(&root)
            || root
                .parent()
                .is_some_and(|legacy_root| is_legacy_workspace_path(path, legacy_root))
    })
}

/// Whether an existing projectless workspace still uses the pre-`projects/`
/// layout and should be moved by the daemon.
pub fn needs_migration(path: &Path) -> bool {
    let Some(root) = workspace_root() else {
        return false;
    };
    !path.starts_with(&root)
        && root
            .parent()
            .is_some_and(|legacy_root| is_legacy_workspace_path(path, legacy_root))
}

fn is_legacy_workspace_path(path: &Path, legacy_root: &Path) -> bool {
    if path == legacy_root {
        return true;
    }
    let Some(date) = path
        .strip_prefix(legacy_root)
        .ok()
        .and_then(|relative| relative.components().next())
        .and_then(|component| component.as_os_str().to_str())
    else {
        return false;
    };
    is_date_component(date)
}

pub fn is_legacy_root_path(path: &Path) -> bool {
    workspace_root()
        .is_some_and(|root| root.parent().is_some_and(|legacy_root| path == legacy_root))
}

pub fn create_workspace(prompt: Option<&str>) -> io::Result<Workspace> {
    let root = workspace_root().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "could not locate the home directory for ~/.waku/projects",
        )
    })?;
    create_workspace_in(&root, Local::now().date_naive(), None, prompt)
}

/// Move one old dated workspace from `~/.waku/<date>/<slug>` into
/// `~/.waku/projects/<date>/<slug>` without copying its contents through the
/// client. The oldest layout used `~/.waku` itself; that path contains Waku's
/// configuration now, so it receives a fresh private workspace instead of
/// moving the configuration directory.
pub fn migrate_workspace(path: &Path) -> io::Result<Workspace> {
    let root = workspace_root().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "could not locate the home directory for ~/.waku/projects",
        )
    })?;
    migrate_workspace_in(&root, path)
}

fn migrate_workspace_in(root: &Path, path: &Path) -> io::Result<Workspace> {
    if path.starts_with(root) {
        validate_real_directory(path)?;
        return Ok(Workspace {
            cwd: path.to_owned(),
            workspace_root: root.to_owned(),
        });
    }
    let legacy_root = root.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid projectless workspace root",
        )
    })?;
    if path == legacy_root {
        return create_workspace_in(root, Local::now().date_naive(), None, None);
    }

    let relative = path.strip_prefix(legacy_root).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "path is not a legacy projectless workspace",
        )
    })?;
    let components = relative.components().collect::<Vec<_>>();
    if components.len() != 2
        || components
            .iter()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
        || !is_date_component(components[0].as_os_str().to_string_lossy().as_ref())
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "path is not a dated legacy projectless workspace",
        ));
    }
    validate_real_directory(path)?;
    ensure_real_directory(root)?;
    let date_directory = root.join(components[0].as_os_str());
    ensure_real_directory(&date_directory)?;
    let original_name = components[1].as_os_str().to_string_lossy();
    for index in 0..MAX_NUMBERED_CANDIDATES {
        let name = if index == 0 {
            original_name.to_string()
        } else {
            format!("{original_name}-{}", index + 1)
        };
        let destination = date_directory.join(name);
        if destination.exists() {
            continue;
        }
        match fs::rename(path, &destination) {
            Ok(()) => {
                return Ok(Workspace {
                    cwd: destination,
                    workspace_root: root.to_owned(),
                });
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "unable to allocate a destination for the legacy projectless workspace",
    ))
}

fn is_date_component(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| matches!(index, 4 | 7) || byte.is_ascii_digit())
}

fn workspace_slug(directory_name: Option<&str>, prompt: Option<&str>) -> String {
    let directory_name = directory_name.filter(|value| !value.trim().is_empty());
    let prompt = prompt.filter(|value| !value.trim().is_empty());
    let source = directory_name.or(prompt).unwrap_or_default().to_lowercase();
    let mut words = source
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty());
    let maximum_words = if directory_name.is_some() {
        usize::MAX
    } else {
        6
    };
    let mut slug = words
        .by_ref()
        .take(maximum_words)
        .collect::<Vec<_>>()
        .join("-");
    slug.truncate(MAX_SLUG_BYTES);
    if slug.is_empty() {
        DEFAULT_SLUG.to_owned()
    } else {
        slug
    }
}

fn create_workspace_in(
    root: &Path,
    date: NaiveDate,
    directory_name: Option<&str>,
    prompt: Option<&str>,
) -> io::Result<Workspace> {
    ensure_real_directory(root)?;
    let date_directory = root.join(date.format("%Y-%m-%d").to_string());
    ensure_real_directory(&date_directory)?;
    let slug = workspace_slug(directory_name, prompt);

    for index in 0..MAX_NUMBERED_CANDIDATES {
        let name = if index == 0 {
            slug.clone()
        } else {
            format!("{slug}-{}", index + 1)
        };
        if let Some(workspace) = try_create_workspace(root, &date_directory, &name)? {
            return Ok(workspace);
        }
    }

    for _ in 0..MAX_RANDOM_CANDIDATES {
        let name = format!("{slug}-{}", Uuid::new_v4());
        if let Some(workspace) = try_create_workspace(root, &date_directory, &name)? {
            return Ok(workspace);
        }
    }

    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "unable to create a unique projectless task directory",
    ))
}

fn try_create_workspace(
    root: &Path,
    date_directory: &Path,
    name: &str,
) -> io::Result<Option<Workspace>> {
    let cwd = date_directory.join(name);
    match fs::create_dir(&cwd) {
        Ok(()) => Ok(Some(Workspace {
            cwd,
            workspace_root: root.to_owned(),
        })),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            validate_real_directory(&cwd)?;
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

fn ensure_real_directory(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(_) => validate_real_directory(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::create_dir_all(path)?;
            validate_real_directory(path)
        }
        Err(error) => Err(error),
    }
}

fn validate_real_directory(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "projectless task directory must be a real directory",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_root() -> PathBuf {
        std::env::temp_dir().join(format!("waku-projectless-{}", Uuid::new_v4()))
    }

    #[test]
    fn prompt_slug_matches_codex_word_and_length_rules() {
        assert_eq!(
            workspace_slug(
                None,
                Some("Build A polished, LOCAL coding agent please now")
            ),
            "build-a-polished-local-coding-agent"
        );
        assert_eq!(workspace_slug(None, Some("你好 👋")), "new-chat");
        assert_eq!(
            workspace_slug(Some("Release Candidate Number 12"), Some("ignored prompt")),
            "release-candidate-number-12"
        );
        assert_eq!(
            workspace_slug(Some("  "), Some("Prompt fallback")),
            "prompt-fallback"
        );
        assert_eq!(
            workspace_slug(Some(&"a".repeat(100)), None).len(),
            MAX_SLUG_BYTES
        );
    }

    #[test]
    fn creates_date_and_unique_task_directories_without_split_folders() {
        let root = test_root();
        let date = NaiveDate::from_ymd_opt(2026, 8, 8).unwrap();

        let first = create_workspace_in(&root, date, None, Some("Fix projectless sessions"))
            .expect("first workspace");
        let second = create_workspace_in(&root, date, None, Some("Fix projectless sessions"))
            .expect("second workspace");

        assert_eq!(first.cwd, root.join("2026-08-08/fix-projectless-sessions"));
        assert_eq!(
            second.cwd,
            root.join("2026-08-08/fix-projectless-sessions-2")
        );
        assert_eq!(first.workspace_root, root);
        assert_eq!(fs::read_dir(&first.cwd).unwrap().count(), 0);

        fs::remove_dir_all(&root).ok();
    }

    #[cfg(unix)]
    #[test]
    fn refuses_a_symlinked_workspace_root() {
        use std::os::unix::fs::symlink;

        let parent = test_root();
        let target = parent.join("target");
        let root = parent.join("root");
        fs::create_dir_all(&target).unwrap();
        symlink(&target, &root).unwrap();

        let error = create_workspace_in(
            &root,
            NaiveDate::from_ymd_opt(2026, 8, 8).unwrap(),
            None,
            None,
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);

        fs::remove_dir_all(&parent).ok();
    }

    #[test]
    fn migrates_legacy_workspace_with_contents_under_projects() {
        let legacy_root = test_root();
        let root = legacy_root.join("projects");
        let legacy = legacy_root.join("2026-08-08/fix-projectless-sessions");
        fs::create_dir_all(&legacy).unwrap();
        fs::write(legacy.join("notes.txt"), "kept").unwrap();

        let migrated = migrate_workspace_in(&root, &legacy).unwrap();

        assert_eq!(
            migrated.cwd,
            root.join("2026-08-08/fix-projectless-sessions")
        );
        assert_eq!(
            fs::read_to_string(migrated.cwd.join("notes.txt")).unwrap(),
            "kept"
        );
        assert!(!legacy.exists());
        fs::remove_dir_all(&legacy_root).ok();
    }
}

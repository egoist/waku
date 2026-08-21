//! Daemon-owned Git branch discovery and checkout operations.
//!
//! Every function in this module performs process I/O. Callers must run them
//! from the background executor; render paths consume only the cached
//! [`BranchSnapshot`] values they return.

use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::Output;

#[cfg(unix)]
use std::ffi::OsString;
#[cfg(unix)]
use std::os::unix::ffi::OsStringExt as _;

use anyhow::{Context as _, anyhow, bail};
const MAX_UNTRACKED_FILES: usize = 2_048;
const MAX_UNTRACKED_FILE_BYTES: u64 = 8 * 1_024 * 1_024;
const MAX_UNTRACKED_TOTAL_BYTES: u64 = 32 * 1_024 * 1_024;
const BINARY_PROBE_BYTES: usize = 8_000;

pub use waku_protocol::git::{BranchEntry, BranchSnapshot};

/// Inspect local branches and which worktree, if any, currently owns each.
/// `Ok(None)` means `cwd` is not inside a Git repository.
pub fn inspect(cwd: &Path) -> anyhow::Result<Option<BranchSnapshot>> {
    let repository_output = crate::command_env::plain_command("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(cwd)
        .output()
        .context("failed to execute git")?;
    if !repository_output.status.success() {
        return Ok(None);
    }
    let repository = PathBuf::from(
        String::from_utf8_lossy(&repository_output.stdout)
            .trim()
            .to_owned(),
    );
    let repository = fs::canonicalize(&repository).unwrap_or(repository);

    let current =
        optional_stdout(cwd, &["branch", "--show-current"])?.filter(|branch| !branch.is_empty());
    let detached_head = if current.is_none() {
        Some(git_stdout(cwd, &["rev-parse", "--short", "HEAD"])?).filter(|head| !head.is_empty())
    } else {
        None
    };

    // `%(worktreepath)` is empty for an available branch and points at the
    // owning checkout otherwise. A NUL separates it from the branch name so
    // paths containing spaces need no shell-style decoding.
    let refs = git_stdout(
        cwd,
        &[
            "for-each-ref",
            "--format=%(refname:short)%00%(worktreepath)",
            "refs/heads",
        ],
    )?;
    let mut branches = refs
        .lines()
        .filter_map(|line| {
            let (name, worktree_path) = line.split_once('\0')?;
            if name.is_empty() {
                return None;
            }
            let checked_out_elsewhere = if worktree_path.is_empty() {
                false
            } else {
                let worktree_path = PathBuf::from(worktree_path);
                let worktree_path = fs::canonicalize(&worktree_path).unwrap_or(worktree_path);
                worktree_path != repository
            };
            Some(BranchEntry {
                name: name.to_owned(),
                checked_out_elsewhere,
            })
        })
        .collect::<Vec<_>>();

    branches.sort_by(|left, right| {
        let left_current = current.as_deref() == Some(left.name.as_str());
        let right_current = current.as_deref() == Some(right.name.as_str());
        right_current
            .cmp(&left_current)
            .then_with(|| left.name.cmp(&right.name))
    });

    let remote_default = optional_stdout(
        cwd,
        &[
            "symbolic-ref",
            "--quiet",
            "--short",
            "refs/remotes/origin/HEAD",
        ],
    )?;
    let default_branch = remote_default
        .as_deref()
        .and_then(|branch| branch.strip_prefix("origin/"))
        .filter(|branch| branches.iter().any(|entry| entry.name == *branch))
        .map(str::to_owned)
        .or_else(|| current.clone());
    let (additions, deletions) = worktree_line_counts(&repository);

    Ok(Some(BranchSnapshot {
        repository,
        current,
        detached_head,
        default_branch,
        branches,
        additions,
        deletions,
    }))
}

fn worktree_line_counts(cwd: &Path) -> (u64, u64) {
    let tracked = crate::command_env::plain_command("git")
        .args(["diff", "--numstat", "HEAD", "--"])
        .current_dir(cwd)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map_or((0, 0), |output| numstat_line_counts(&output.stdout));
    (
        tracked.0.saturating_add(untracked_line_additions(cwd)),
        tracked.1,
    )
}

fn numstat_line_counts(output: &[u8]) -> (u64, u64) {
    String::from_utf8_lossy(output)
        .lines()
        .fold((0, 0), |(additions, deletions), line| {
            let mut fields = line.splitn(3, '\t');
            let added = fields.next().and_then(|value| value.parse::<u64>().ok());
            let deleted = fields.next().and_then(|value| value.parse::<u64>().ok());
            match (added, deleted) {
                (Some(added), Some(deleted)) => (
                    additions.saturating_add(added),
                    deletions.saturating_add(deleted),
                ),
                // Binary files are reported as `-\t-` and have no line count.
                _ => (additions, deletions),
            }
        })
}

/// `git diff --numstat` deliberately omits untracked files. Count their text
/// lines as additions so the compact status agrees with the Uncommitted review
/// source that opens when it is clicked. Bounds keep generated trees from
/// turning a background metadata refresh into unbounded work.
fn untracked_line_additions(repository: &Path) -> u64 {
    let Ok(output) = crate::command_env::plain_command("git")
        .args([
            "ls-files",
            "--others",
            "--exclude-standard",
            "--full-name",
            "-z",
            "--",
        ])
        .current_dir(repository)
        .output()
    else {
        return 0;
    };
    if !output.status.success() {
        return 0;
    }

    let mut remaining_bytes = MAX_UNTRACKED_TOTAL_BYTES;
    let mut additions = 0_u64;
    for path in output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .take(MAX_UNTRACKED_FILES)
    {
        if remaining_bytes == 0 {
            break;
        }
        let path = repository.join(path_from_git_bytes(path));
        let (lines, bytes_read) =
            untracked_text_line_count(&path, remaining_bytes.min(MAX_UNTRACKED_FILE_BYTES));
        additions = additions.saturating_add(lines);
        remaining_bytes = remaining_bytes.saturating_sub(bytes_read);
    }
    additions
}

fn untracked_text_line_count(path: &Path, maximum_bytes: u64) -> (u64, u64) {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return (0, 0);
    };
    if metadata.file_type().is_symlink() {
        return (1, 0);
    }
    if !metadata.is_file() || metadata.len() > maximum_bytes {
        return (0, 0);
    }

    let Ok(file) = fs::File::open(path) else {
        return (0, 0);
    };
    let mut content = Vec::with_capacity(metadata.len() as usize);
    if file
        .take(maximum_bytes.saturating_add(1))
        .read_to_end(&mut content)
        .is_err()
    {
        return (0, 0);
    }
    let bytes_read = content.len() as u64;
    if bytes_read > maximum_bytes || content[..content.len().min(BINARY_PROBE_BYTES)].contains(&0) {
        return (0, bytes_read);
    }
    if content.is_empty() {
        return (0, 0);
    }
    let newline_count = content.iter().filter(|byte| **byte == b'\n').count() as u64;
    let trailing_line = u64::from(content.last() != Some(&b'\n'));
    (newline_count.saturating_add(trailing_line), bytes_read)
}

#[cfg(unix)]
fn path_from_git_bytes(path: &[u8]) -> PathBuf {
    PathBuf::from(OsString::from_vec(path.to_vec()))
}

#[cfg(not(unix))]
fn path_from_git_bytes(path: &[u8]) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(path).into_owned())
}

pub fn checkout(cwd: &Path, branch: &str) -> anyhow::Result<BranchSnapshot> {
    let output = crate::command_env::plain_command("git")
        .args(["switch", "--"])
        .arg(branch)
        .current_dir(cwd)
        .output()
        .context("failed to execute git switch")?;
    if !output.status.success() {
        bail!("{}", command_error(&output));
    }
    inspect(cwd)?.ok_or_else(|| anyhow!("the workspace is no longer a Git repository"))
}

pub fn create_and_checkout(cwd: &Path, branch: &str) -> anyhow::Result<BranchSnapshot> {
    let branch = branch.trim();
    if branch.is_empty() {
        bail!("enter a branch name");
    }
    let validation = crate::command_env::plain_command("git")
        .args(["check-ref-format", "--branch"])
        .arg(branch)
        .current_dir(cwd)
        .output()
        .context("failed to validate the branch name")?;
    if !validation.status.success() {
        bail!("{}", command_error(&validation));
    }
    let output = crate::command_env::plain_command("git")
        .args(["switch", "-c"])
        .arg(branch)
        .current_dir(cwd)
        .output()
        .context("failed to execute git switch")?;
    if !output.status.success() {
        bail!("{}", command_error(&output));
    }
    inspect(cwd)?.ok_or_else(|| anyhow!("the workspace is no longer a Git repository"))
}

fn git_stdout(cwd: &Path, args: &[&str]) -> anyhow::Result<String> {
    let output = crate::command_env::plain_command("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .context("failed to execute git")?;
    if !output.status.success() {
        bail!("{}", command_error(&output));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn optional_stdout(cwd: &Path, args: &[&str]) -> anyhow::Result<Option<String>> {
    let output = crate::command_env::plain_command("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .context("failed to execute git")?;
    if output.status.success() {
        return Ok(Some(
            String::from_utf8_lossy(&output.stdout).trim().to_owned(),
        ));
    }
    if output.status.code() == Some(1) {
        return Ok(None);
    }
    bail!("{}", command_error(&output))
}

fn command_error(output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if stderr.is_empty() {
        format!("git exited with {}", output.status)
    } else {
        stderr
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn run_git(cwd: &Path, args: &[&str]) {
        let output = crate::command_env::plain_command("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", command_error(&output));
    }

    fn repository() -> PathBuf {
        let root = std::env::temp_dir().join(format!("waku-branch-test-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        run_git(&root, &["init", "-b", "main"]);
        fs::write(root.join("README.md"), "main\n").unwrap();
        run_git(&root, &["add", "."]);
        run_git(
            &root,
            &[
                "-c",
                "user.name=Waku Tests",
                "-c",
                "user.email=waku@example.com",
                "commit",
                "-m",
                "initial",
            ],
        );
        run_git(&root, &["branch", "feature"]);
        root
    }

    #[test]
    fn discovers_current_and_worktree_owned_branches() {
        let repository = repository();
        let other = repository.with_extension("other-worktree");
        run_git(
            &repository,
            &["worktree", "add", "-b", "occupied", other.to_str().unwrap()],
        );

        let snapshot = inspect(&repository).unwrap().unwrap();
        assert_eq!(snapshot.current.as_deref(), Some("main"));
        assert_eq!(snapshot.branches[0].name, "main");
        assert!(
            snapshot
                .branches
                .iter()
                .find(|branch| branch.name == "occupied")
                .unwrap()
                .checked_out_elsewhere
        );
        assert!(
            !snapshot
                .branches
                .iter()
                .find(|branch| branch.name == "feature")
                .unwrap()
                .checked_out_elsewhere
        );
    }

    #[test]
    fn switches_and_creates_branches() {
        let repository = repository();
        let switched = checkout(&repository, "feature").unwrap();
        assert_eq!(switched.current.as_deref(), Some("feature"));

        let created = create_and_checkout(&repository, "topic/new-picker").unwrap();
        assert_eq!(created.current.as_deref(), Some("topic/new-picker"));
        assert!(
            created
                .branches
                .iter()
                .any(|branch| branch.name == "topic/new-picker")
        );
    }

    #[test]
    fn counts_tracked_worktree_changes() {
        let repository = repository();
        fs::write(repository.join("README.md"), "main\nsecond\n").unwrap();

        let snapshot = inspect(&repository).unwrap().unwrap();
        assert_eq!((snapshot.additions, snapshot.deletions), (1, 0));
    }

    #[test]
    fn counts_tracked_and_untracked_changes_in_a_linked_worktree() {
        let repository = repository();
        let worktree = repository.with_extension("linked-worktree");
        run_git(
            &repository,
            &[
                "worktree",
                "add",
                "-b",
                "linked-worktree",
                worktree.to_str().unwrap(),
            ],
        );
        fs::write(worktree.join("README.md"), "main\nsecond\n").unwrap();
        fs::write(worktree.join("new file.txt"), "first\nsecond").unwrap();
        fs::write(worktree.join("binary.dat"), b"binary\0content\n").unwrap();

        let snapshot = inspect(&worktree).unwrap().unwrap();
        assert_eq!((snapshot.additions, snapshot.deletions), (3, 0));
    }
}

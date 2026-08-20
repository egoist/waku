//! Daemon-owned isolated Git worktrees for tasks.
//!
//! A draft records only the user's choice. The first submission creates the
//! worktree beneath `~/.waku/worktrees`, then every workspace consumer uses
//! the returned project-relative path for the lifetime of the task.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

use anyhow::{Context as _, anyhow, bail};
use uuid::Uuid;

const DEFAULT_SLUG: &str = "new-worktree";
const MAX_SLUG_BYTES: usize = 48;
const MAX_CANDIDATES: usize = 100;

pub use waku_protocol::git::CreatedWorktree;

pub fn create(
    project_path: &Path,
    project_id: Uuid,
    session_id: Uuid,
    prompt: &str,
    base_branch: Option<&str>,
) -> anyhow::Result<CreatedWorktree> {
    let root = dirs::home_dir()
        .ok_or_else(|| anyhow!("could not locate the home directory for ~/.waku/worktrees"))?
        .join(".waku/worktrees");
    create_in(
        project_path,
        &root,
        project_id,
        session_id,
        prompt,
        base_branch,
    )
}

fn create_in(
    project_path: &Path,
    worktree_root: &Path,
    project_id: Uuid,
    session_id: Uuid,
    prompt: &str,
    requested_base: Option<&str>,
) -> anyhow::Result<CreatedWorktree> {
    let project_path = fs::canonicalize(project_path)
        .with_context(|| format!("could not open project {}", project_path.display()))?;
    let repository = git_stdout(&project_path, &["rev-parse", "--show-toplevel"])
        .context("new worktrees require a Git repository")?;
    let repository = fs::canonicalize(PathBuf::from(repository.trim()))
        .context("could not resolve the Git repository root")?;
    let project_relative = project_path
        .strip_prefix(&repository)
        .context("the project is outside its Git repository root")?
        .to_owned();
    let base_ref = requested_base
        .map(str::trim)
        .filter(|branch| !branch.is_empty())
        .map(str::to_owned)
        .map(Ok)
        .unwrap_or_else(|| default_base_ref(&repository))?;
    git_stdout(
        &repository,
        &["rev-parse", "--verify", &format!("{base_ref}^{{commit}}")],
    )
    .with_context(|| format!("base branch `{base_ref}` is unavailable"))?;
    let project_worktrees = worktree_root.join(project_id.to_string());
    fs::create_dir_all(&project_worktrees).with_context(|| {
        format!(
            "could not create the worktree directory {}",
            project_worktrees.display()
        )
    })?;

    let slug = worktree_slug(prompt);
    for index in 0..MAX_CANDIDATES {
        let name = candidate_name(&slug, index);
        let branch = format!("waku/{name}");
        let path = project_worktrees.join(&name);
        if path.exists() || local_branch_exists(&repository, &branch)? {
            continue;
        }

        let output = crate::command_env::plain_command("git")
            .args(["worktree", "add", "-b"])
            .arg(&branch)
            .arg(&path)
            .arg(&base_ref)
            .current_dir(&repository)
            .output()
            .context("failed to execute git worktree add")?;
        if !output.status.success() {
            bail!("{}", command_error(&output));
        }

        let project_path = path.join(&project_relative);
        if !project_path.is_dir() {
            bail!(
                "Git created the worktree, but its project directory is missing: {}",
                project_path.display()
            );
        }
        return Ok(CreatedWorktree {
            path: project_path,
            branch,
        });
    }

    // A UUID fallback makes the final attempt independent of human-readable
    // name collisions while keeping the common path and branch pleasant.
    let fallback = format!("{slug}-{}", &session_id.simple().to_string()[..8]);
    let branch = format!("waku/{fallback}");
    let path = project_worktrees.join(&fallback);
    if path.exists() || local_branch_exists(&repository, &branch)? {
        bail!("could not allocate a unique Git worktree name");
    }
    let output = crate::command_env::plain_command("git")
        .args(["worktree", "add", "-b"])
        .arg(&branch)
        .arg(&path)
        .arg(&base_ref)
        .current_dir(&repository)
        .output()
        .context("failed to execute git worktree add")?;
    if !output.status.success() {
        bail!("{}", command_error(&output));
    }
    Ok(CreatedWorktree {
        path: path.join(project_relative),
        branch,
    })
}

/// Prefer the local branch named by `origin/HEAD`, so a worktree starts from
/// the default branch without fetching or mutating the user's ordinary
/// checkout. Repositories without that metadata fall back to their current
/// branch, then detached `HEAD`.
fn default_base_ref(repository: &Path) -> anyhow::Result<String> {
    if let Some(remote_default) = git_optional_stdout(
        repository,
        &[
            "symbolic-ref",
            "--quiet",
            "--short",
            "refs/remotes/origin/HEAD",
        ],
    )? {
        if let Some(local_default) = remote_default.trim().strip_prefix("origin/")
            && local_branch_exists(repository, local_default)?
        {
            return Ok(local_default.to_owned());
        }
        return Ok(remote_default.trim().to_owned());
    }
    if let Some(branch) = git_optional_stdout(repository, &["branch", "--show-current"])?
        && !branch.trim().is_empty()
    {
        return Ok(branch.trim().to_owned());
    }
    git_stdout(repository, &["rev-parse", "--verify", "HEAD"])?;
    Ok("HEAD".to_owned())
}

fn local_branch_exists(repository: &Path, branch: &str) -> anyhow::Result<bool> {
    let output = crate::command_env::plain_command("git")
        .args(["show-ref", "--verify", "--quiet"])
        .arg(format!("refs/heads/{branch}"))
        .current_dir(repository)
        .output()
        .context("failed to inspect Git branches")?;
    match output.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => bail!("{}", command_error(&output)),
    }
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

fn git_optional_stdout(cwd: &Path, args: &[&str]) -> anyhow::Result<Option<String>> {
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

fn candidate_name(slug: &str, index: usize) -> String {
    if index == 0 {
        slug.to_owned()
    } else {
        format!("{slug}-{}", index + 1)
    }
}

fn worktree_slug(prompt: &str) -> String {
    let mut slug = prompt
        .to_ascii_lowercase()
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
        .take(6)
        .collect::<Vec<_>>()
        .join("-");
    slug.truncate(MAX_SLUG_BYTES);
    if slug.is_empty() {
        DEFAULT_SLUG.to_owned()
    } else {
        slug
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_git(cwd: &Path, args: &[&str]) {
        let output = crate::command_env::plain_command("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", command_error(&output));
    }

    #[test]
    fn creates_isolated_worktrees_from_the_default_branch() {
        let root = std::env::temp_dir().join(format!("waku-worktree-test-{}", Uuid::new_v4()));
        let repository = root.join("repository");
        let project = repository.join("packages/app");
        fs::create_dir_all(&project).unwrap();
        run_git(&repository, &["init", "-b", "main"]);
        fs::write(project.join("README.md"), "main\n").unwrap();
        run_git(&repository, &["add", "."]);
        run_git(
            &repository,
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
        run_git(
            &repository,
            &["update-ref", "refs/remotes/origin/main", "refs/heads/main"],
        );
        run_git(
            &repository,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/main",
            ],
        );
        run_git(&repository, &["checkout", "-b", "feature"]);
        fs::write(project.join("README.md"), "feature\n").unwrap();
        run_git(&repository, &["add", "."]);
        run_git(
            &repository,
            &[
                "-c",
                "user.name=Waku Tests",
                "-c",
                "user.email=waku@example.com",
                "commit",
                "-m",
                "feature",
            ],
        );

        let worktree_root = root.join("worktrees");
        let project_id = Uuid::new_v4();
        let first = create_in(
            &project,
            &worktree_root,
            project_id,
            Uuid::new_v4(),
            "Build a project selector",
            None,
        )
        .unwrap();
        assert_eq!(first.branch, "waku/build-a-project-selector");
        assert_eq!(
            fs::read_to_string(first.path.join("README.md")).unwrap(),
            "main\n"
        );
        fs::write(first.path.join("README.md"), "worktree\n").unwrap();
        assert_eq!(
            fs::read_to_string(project.join("README.md")).unwrap(),
            "feature\n"
        );

        let second = create_in(
            &project,
            &worktree_root,
            project_id,
            Uuid::new_v4(),
            "Build a project selector",
            None,
        )
        .unwrap();
        assert_eq!(second.branch, "waku/build-a-project-selector-2");

        let from_feature = create_in(
            &project,
            &worktree_root,
            project_id,
            Uuid::new_v4(),
            "Use selected base",
            Some("feature"),
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(from_feature.path.join("README.md")).unwrap(),
            "feature\n"
        );

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn slug_is_short_and_branch_safe() {
        assert_eq!(worktree_slug("你好 👋"), DEFAULT_SLUG);
        assert_eq!(
            worktree_slug("Fix Project/Worktree Picker!"),
            "fix-project-worktree-picker"
        );
        assert!(worktree_slug(&"a".repeat(100)).len() <= MAX_SLUG_BYTES);
    }
}

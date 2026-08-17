//! Daemon-owned Git commit/push operations and one-shot agent commit-message
//! generation. Every entry point performs process I/O and must run on the
//! background executor; render code consumes only the returned snapshots.

use std::ffi::OsString;
use std::fs;
use std::io::Read;
use std::path::Path;
#[cfg(test)]
use std::path::PathBuf;
use std::process::{Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context as _, anyhow, bail};
use uuid::Uuid;

use crate::model::ProviderKind;
pub use waku_protocol::git::{AgentInvocation, CommitSnapshot as Snapshot};

const GIT_TIMEOUT: Duration = Duration::from_secs(120);
const AGENT_TIMEOUT: Duration = Duration::from_secs(180);
const PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(40);
const MAX_DIFF_BYTES: usize = 96 * 1024;
const MAX_STDOUT_BYTES: usize = 1024 * 1024;
const MAX_STDERR_BYTES: usize = 256 * 1024;
const MAX_ERROR_CHARS: usize = 4_000;

// A commit subject is a fixed classification over a diff that is already in the
// prompt, so it does not need — or benefit from — the model the task runs on.
// Where a provider exposes a cheap tier by name, generation is pinned to it and
// to the lowest effort that tier's API accepts, whatever the session selected.
const CLAUDE_COMMIT_MODEL: &str = "claude-haiku-4-5";
const CLAUDE_COMMIT_EFFORT: &str = "low";
const CODEX_COMMIT_MODEL: &str = "gpt-5.6-luna";
// `codex exec` has no effort flag, so the config override is the only route.
// `none` is the floor: `minimal` is rejected by the API for this model with
// `unsupported_value`, listing `none` as the lowest it accepts.
const CODEX_COMMIT_EFFORT: &str = r#"model_reasoning_effort="none""#;

struct CapturedOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

pub fn inspect(cwd: &Path) -> anyhow::Result<Snapshot> {
    ensure_repository(cwd)?;
    let branch = git_optional_stdout(cwd, &["branch", "--show-current"])?
        .filter(|branch| !branch.is_empty())
        .or_else(|| {
            git_optional_stdout(cwd, &["rev-parse", "--short", "HEAD"])
                .ok()
                .flatten()
        })
        .unwrap_or_else(|| "HEAD".to_owned());
    let status = git_stdout(cwd, &["status", "--porcelain=v1", "--untracked-files=all"])?;
    let (has_staged, has_unstaged) = status_flags(&status);
    let (additions, deletions) = numstat(cwd, &["diff", "--numstat", "HEAD", "--"])
        .unwrap_or_else(|_| combined_numstat(cwd));
    let (staged_additions, staged_deletions) =
        numstat(cwd, &["diff", "--cached", "--numstat", "--"]).unwrap_or_default();
    let can_push = push_target(cwd, &branch)?
        .and_then(|target| {
            git_optional_stdout(cwd, &["rev-list", "--count", &format!("{target}..HEAD")])
                .ok()
                .flatten()
        })
        .and_then(|count| count.parse::<u64>().ok())
        .is_some_and(|count| count > 0)
        || (upstream(cwd)?.is_none() && remote_for_branch(cwd, &branch)?.is_some());
    Ok(Snapshot {
        branch,
        additions,
        deletions,
        staged_additions,
        staged_deletions,
        has_staged,
        has_unstaged,
        can_push,
    })
}

pub fn generate_message(
    cwd: &Path,
    include_unstaged: bool,
    invocation: &AgentInvocation,
) -> anyhow::Result<String> {
    let prompt = commit_prompt(cwd, include_unstaged)?;
    let amp_settings = if invocation.provider == ProviderKind::Amp {
        let path = std::env::temp_dir().join(format!("waku-amp-commit-{}.json", Uuid::new_v4()));
        fs::write(
            &path,
            r#"{"amp.tools.enable":[],"amp.notifications.enabled":false,"amp.skills.disableClaudeCodeSkills":true}"#,
        )
        .context("could not prepare Amp commit-message settings")?;
        Some(path)
    } else {
        None
    };
    let args = agent_arguments(
        invocation.provider,
        invocation.model.as_deref(),
        invocation.reasoning_effort.as_deref(),
        &prompt,
        amp_settings.as_deref(),
    );
    let mut command = crate::command_env::command(&invocation.binary);
    command
        .args(args)
        .current_dir(cwd)
        .env("NO_COLOR", "1")
        .env("CI", "1");
    let result = run_capture(&mut command, AGENT_TIMEOUT).with_context(|| {
        format!(
            "{} could not generate a commit message",
            invocation.provider.display_name()
        )
    });
    if let Some(path) = amp_settings {
        let _ = fs::remove_file(path);
    }
    let output = result?;
    if !output.status.success() {
        bail!(
            "{} could not generate a commit message: {}",
            invocation.provider.display_name(),
            command_error(&output)
        );
    }
    normalize_message(&String::from_utf8_lossy(&output.stdout)).ok_or_else(|| {
        anyhow!(
            "{} returned no commit message",
            invocation.provider.display_name()
        )
    })
}

pub fn commit(cwd: &Path, message: &str, include_unstaged: bool) -> anyhow::Result<()> {
    ensure_repository(cwd)?;
    let message = normalize_message(message).ok_or_else(|| anyhow!("enter a commit message"))?;
    if include_unstaged {
        git_success(cwd, &["add", "-A", "--", "."])?;
    }
    let staged = git_capture(cwd, &["diff", "--cached", "--quiet", "--"])?;
    match staged.status.code() {
        Some(1) => {}
        Some(0) => bail!("there are no staged changes to commit"),
        _ => bail!(
            "could not inspect staged changes: {}",
            command_error(&staged)
        ),
    }
    git_success(cwd, &["commit", "-m", &message])?;
    Ok(())
}

pub fn push(cwd: &Path) -> anyhow::Result<()> {
    ensure_repository(cwd)?;
    if upstream(cwd)?.is_some() {
        git_success(cwd, &["push"])?;
        return Ok(());
    }
    let branch = git_optional_stdout(cwd, &["branch", "--show-current"])?
        .filter(|branch| !branch.is_empty())
        .ok_or_else(|| anyhow!("cannot push a detached HEAD"))?;
    let remote = remote_for_branch(cwd, &branch)?
        .ok_or_else(|| anyhow!("no Git remote is configured for this branch"))?;
    git_success(cwd, &["push", "--set-upstream", &remote, &branch])?;
    Ok(())
}

fn commit_prompt(cwd: &Path, include_unstaged: bool) -> anyhow::Result<String> {
    ensure_repository(cwd)?;
    let staged_status = git_stdout(cwd, &["diff", "--cached", "--name-status", "--"])?;
    let staged = git_stdout(
        cwd,
        &["diff", "--cached", "--no-ext-diff", "--no-color", "--"],
    )?;
    let (status, unstaged) = if include_unstaged {
        (
            git_stdout(cwd, &["status", "--short", "--untracked-files=all"])?,
            git_stdout(cwd, &["diff", "--no-ext-diff", "--no-color", "--"])?,
        )
    } else {
        (staged_status, String::new())
    };
    if status.trim().is_empty() && staged.trim().is_empty() && unstaged.trim().is_empty() {
        bail!("there are no changes to describe");
    }
    let mut context = format!("Git status:\n{status}\n\nStaged diff:\n{staged}");
    if include_unstaged {
        context.push_str("\n\nUnstaged diff (will be included):\n");
        context.push_str(&unstaged);
    }
    let (context, truncated) = truncate_utf8(context, MAX_DIFF_BYTES);
    Ok(format!(
        "Generate a concise Git commit subject for the changes below.\n\
         Return exactly one line and nothing else: no quotes, Markdown, prefix, explanation, or trailing period.\n\
         Use imperative mood and at most 72 characters. Do not call tools; all context is included here.{}\n\n{}",
        if truncated {
            " The diff was truncated, so summarize only what is supported by the visible context."
        } else {
            ""
        },
        context
    ))
}

fn agent_arguments(
    provider: ProviderKind,
    model: Option<&str>,
    reasoning_effort: Option<&str>,
    prompt: &str,
    amp_settings: Option<&Path>,
) -> Vec<OsString> {
    let mut args = Vec::<OsString>::new();
    fn push(args: &mut Vec<OsString>, value: &str) {
        args.push(OsString::from(value));
    }
    match provider {
        ProviderKind::Amp => {
            push(&mut args, "--execute");
            push(&mut args, "--no-color");
            push(&mut args, "--no-ide");
            push(&mut args, "--no-notifications");
            if let Some(settings) = amp_settings {
                push(&mut args, "--settings-file");
                args.push(settings.as_os_str().to_owned());
            }
            if let Some(model) = model {
                push(&mut args, "--mode");
                push(&mut args, model);
            }
            if let Some(effort) = reasoning_effort {
                push(&mut args, "--effort");
                push(&mut args, effort);
            }
        }
        ProviderKind::Claude => {
            push(&mut args, "--print");
            push(&mut args, "--output-format");
            push(&mut args, "text");
            push(&mut args, "--permission-mode");
            push(&mut args, "plan");
            push(&mut args, "--tools");
            push(&mut args, "");
            push(&mut args, "--disable-slash-commands");
            push(&mut args, "--no-session-persistence");
            push(&mut args, "--no-chrome");
            push(&mut args, "--model");
            push(&mut args, CLAUDE_COMMIT_MODEL);
            push(&mut args, "--effort");
            push(&mut args, CLAUDE_COMMIT_EFFORT);
        }
        ProviderKind::Codex => {
            push(&mut args, "exec");
            push(&mut args, "--sandbox");
            push(&mut args, "read-only");
            push(&mut args, "--ephemeral");
            push(&mut args, "--color");
            push(&mut args, "never");
            push(&mut args, "--skip-git-repo-check");
            push(&mut args, "--model");
            push(&mut args, CODEX_COMMIT_MODEL);
            push(&mut args, "-c");
            push(&mut args, CODEX_COMMIT_EFFORT);
        }
        ProviderKind::Cursor => {
            push(&mut args, "--print");
            push(&mut args, "--output-format");
            push(&mut args, "text");
            push(&mut args, "--mode");
            push(&mut args, "ask");
            push(&mut args, "--sandbox");
            push(&mut args, "enabled");
            push(&mut args, "--trust");
            if let Some(model) = model {
                push(&mut args, "--model");
                push(&mut args, model);
            }
        }
        ProviderKind::DeepSeek => {
            // The headless profile is Harness's one-shot, stdout-only client.
            // The commit prompt embeds all context and explicitly forbids tools.
            push(&mut args, "--profile");
            push(&mut args, "headless");
        }
        ProviderKind::Fx => {
            push(&mut args, "ask");
            push(&mut args, "--no-save");
            push(&mut args, "--no-color");
            push(&mut args, "--");
        }
        ProviderKind::OpenCode => {
            push(&mut args, "run");
            push(&mut args, "--pure");
            push(&mut args, "--agent");
            push(&mut args, "plan");
            if let Some(model) = model {
                push(&mut args, "--model");
                push(&mut args, model);
            }
            if let Some(effort) = reasoning_effort {
                push(&mut args, "--variant");
                push(&mut args, effort);
            }
        }
        ProviderKind::Grok => {
            push(&mut args, "--single");
            push(&mut args, prompt);
            push(&mut args, "--output-format");
            push(&mut args, "plain");
            push(&mut args, "--permission-mode");
            push(&mut args, "plan");
            push(&mut args, "--tools");
            push(&mut args, "");
            push(&mut args, "--no-memory");
            push(&mut args, "--no-subagents");
            push(&mut args, "--disable-web-search");
            push(&mut args, "--verbatim");
            if let Some(model) = model {
                push(&mut args, "--model");
                push(&mut args, model);
            }
            if let Some(effort) = reasoning_effort {
                push(&mut args, "--reasoning-effort");
                push(&mut args, effort);
            }
            return args;
        }
        // Kimi carries the prompt as `--prompt`'s value rather than a trailing
        // positional, so it returns early. It has no tool or session switches
        // to turn off; the commit prompt is what forbids tool use.
        ProviderKind::Kimi => {
            push(&mut args, "--prompt");
            push(&mut args, prompt);
            push(&mut args, "--output-format");
            push(&mut args, "text");
            if let Some(model) = model {
                push(&mut args, "--model");
                push(&mut args, model);
            }
            return args;
        }
        // Oh My Pi rejects unknown flags outright, so it gets its own list
        // rather than Pi's: context files are `--no-rules`, and it has no
        // prompt-template or project-trust switch to turn off.
        ProviderKind::OhMyPi => {
            push(&mut args, "--print");
            push(&mut args, "--no-session");
            push(&mut args, "--no-tools");
            push(&mut args, "--no-rules");
            push(&mut args, "--no-extensions");
            push(&mut args, "--no-skills");
            if let Some(model) = model {
                push(&mut args, "--model");
                push(&mut args, model);
            }
            if let Some(effort) = reasoning_effort {
                push(&mut args, "--thinking");
                push(&mut args, effort);
            }
        }
        ProviderKind::Pi => {
            push(&mut args, "--print");
            push(&mut args, "--no-session");
            push(&mut args, "--no-tools");
            push(&mut args, "--no-context-files");
            push(&mut args, "--no-extensions");
            push(&mut args, "--no-skills");
            push(&mut args, "--no-prompt-templates");
            push(&mut args, "--no-approve");
            if let Some(model) = model {
                push(&mut args, "--model");
                push(&mut args, model);
            }
            if let Some(effort) = reasoning_effort {
                push(&mut args, "--thinking");
                push(&mut args, effort);
            }
        }
    }
    push(&mut args, prompt);
    args
}

fn normalize_message(output: &str) -> Option<String> {
    let clean = strip_ansi(output);
    let mut candidates = clean
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && *line != "```")
        .filter(|line| !line.starts_with("[tool]") && !line.starts_with("[thinking]"))
        .collect::<Vec<_>>();
    let candidate = candidates.pop()?.trim_matches('`').trim();
    let candidate = candidate
        .strip_prefix("Commit message:")
        .or_else(|| candidate.strip_prefix("Commit subject:"))
        .unwrap_or(candidate)
        .trim()
        .trim_matches(['\"', '\'', '`'])
        .trim_end_matches('.')
        .trim();
    if candidate.is_empty() {
        return None;
    }
    Some(candidate.chars().take(200).collect())
}

fn strip_ansi(text: &str) -> String {
    let mut clean = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(character) = chars.next() {
        if character == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for next in chars.by_ref() {
                if ('@'..='~').contains(&next) {
                    break;
                }
            }
        } else {
            clean.push(character);
        }
    }
    clean
}

fn truncate_utf8(mut value: String, limit: usize) -> (String, bool) {
    if value.len() <= limit {
        return (value, false);
    }
    let mut end = limit;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value.truncate(end);
    value.push_str("\n\n[diff truncated]");
    (value, true)
}

fn ensure_repository(cwd: &Path) -> anyhow::Result<()> {
    git_success(cwd, &["rev-parse", "--git-dir"]).map(|_| ())
}

fn status_flags(status: &str) -> (bool, bool) {
    status
        .lines()
        .fold((false, false), |(staged, unstaged), line| {
            let bytes = line.as_bytes();
            if bytes.len() < 2 {
                return (staged, unstaged);
            }
            let x = bytes[0];
            let y = bytes[1];
            (
                staged || (x != b' ' && x != b'?'),
                unstaged || y != b' ' || x == b'?',
            )
        })
}

fn combined_numstat(cwd: &Path) -> (u64, u64) {
    let staged = numstat(cwd, &["diff", "--cached", "--numstat", "--"]).unwrap_or_default();
    let unstaged = numstat(cwd, &["diff", "--numstat", "--"]).unwrap_or_default();
    (staged.0 + unstaged.0, staged.1 + unstaged.1)
}

fn numstat(cwd: &Path, args: &[&str]) -> anyhow::Result<(u64, u64)> {
    let output = git_stdout(cwd, args)?;
    Ok(output.lines().fold((0, 0), |(additions, deletions), line| {
        let mut fields = line.splitn(3, '\t');
        let added = fields.next().and_then(|value| value.parse::<u64>().ok());
        let deleted = fields.next().and_then(|value| value.parse::<u64>().ok());
        match (added, deleted) {
            (Some(added), Some(deleted)) => (additions + added, deletions + deleted),
            _ => (additions, deletions),
        }
    }))
}

fn upstream(cwd: &Path) -> anyhow::Result<Option<String>> {
    git_optional_stdout(
        cwd,
        &[
            "rev-parse",
            "--abbrev-ref",
            "--symbolic-full-name",
            "@{upstream}",
        ],
    )
}

fn remote_for_branch(cwd: &Path, branch: &str) -> anyhow::Result<Option<String>> {
    let remotes = git_stdout(cwd, &["remote"])?;
    let mut remotes = remotes.lines().filter(|remote| !remote.is_empty());
    if remotes.clone().any(|remote| remote == "origin") {
        return Ok(Some("origin".to_owned()));
    }
    let _ = branch;
    Ok(remotes.next().map(str::to_owned))
}

fn push_target(cwd: &Path, branch: &str) -> anyhow::Result<Option<String>> {
    if let Some(upstream) = upstream(cwd)? {
        return Ok(Some(upstream));
    }
    let Some(remote) = remote_for_branch(cwd, branch)? else {
        return Ok(None);
    };
    let target = format!("{remote}/{branch}");
    let exists = git_capture(
        cwd,
        &[
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/remotes/{target}"),
        ],
    )?;
    Ok(exists.status.success().then_some(target))
}

fn git_stdout(cwd: &Path, args: &[&str]) -> anyhow::Result<String> {
    let output = git_success(cwd, args)?;
    Ok(String::from_utf8_lossy(&output.stdout)
        .trim_end()
        .to_owned())
}

fn git_optional_stdout(cwd: &Path, args: &[&str]) -> anyhow::Result<Option<String>> {
    let output = git_capture(cwd, args)?;
    if output.status.success() {
        return Ok(Some(
            String::from_utf8_lossy(&output.stdout).trim().to_owned(),
        ));
    }
    if output.status.code() == Some(1) || output.status.code() == Some(128) {
        return Ok(None);
    }
    bail!("Git command failed: {}", command_error(&output))
}

fn git_success(cwd: &Path, args: &[&str]) -> anyhow::Result<CapturedOutput> {
    let output = git_capture(cwd, args)?;
    if output.status.success() {
        Ok(output)
    } else {
        bail!("Git command failed: {}", command_error(&output))
    }
}

fn git_capture(cwd: &Path, args: &[&str]) -> anyhow::Result<CapturedOutput> {
    let mut command = crate::command_env::command("git");
    command
        .args(args)
        .current_dir(cwd)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_EDITOR", "true");
    run_capture(&mut command, GIT_TIMEOUT)
}

fn run_capture(command: &mut Command, timeout: Duration) -> anyhow::Result<CapturedOutput> {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        // Agent CLIs can start helpers. Give the invocation its own group so
        // a timeout does not leave those descendants running in the workspace.
        command.process_group(0);
    }
    let command = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = crate::command_env::spawn(command).context("could not start process")?;
    let stdout = child.stdout.take().context("process stdout unavailable")?;
    let stderr = child.stderr.take().context("process stderr unavailable")?;
    let stdout_reader = thread::spawn(move || read_bounded(stdout, MAX_STDOUT_BYTES));
    let stderr_reader = thread::spawn(move || read_bounded(stderr, MAX_STDERR_BYTES));
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait().context("could not wait for process")? {
            break status;
        }
        if Instant::now() >= deadline {
            #[cfg(unix)]
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGKILL);
            }
            let _ = child.kill();
            let _ = child.wait();
            let _ = stdout_reader.join();
            let _ = stderr_reader.join();
            bail!("process timed out after {} seconds", timeout.as_secs());
        }
        thread::sleep(PROCESS_POLL_INTERVAL);
    };
    Ok(CapturedOutput {
        status,
        stdout: stdout_reader.join().unwrap_or_default(),
        stderr: stderr_reader.join().unwrap_or_default(),
    })
}

fn read_bounded(mut reader: impl Read, limit: usize) -> Vec<u8> {
    let mut kept = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        let Ok(read) = reader.read(&mut chunk) else {
            break;
        };
        if read == 0 {
            break;
        }
        let remaining = limit.saturating_sub(kept.len());
        kept.extend_from_slice(&chunk[..read.min(remaining)]);
    }
    kept
}

fn command_error(output: &CapturedOutput) -> String {
    let stderr = strip_ansi(&String::from_utf8_lossy(&output.stderr));
    let stderr = stderr.trim();
    if stderr.is_empty() {
        format!("process exited with {}", output.status)
    } else {
        let mut displayed = stderr.chars().take(MAX_ERROR_CHARS).collect::<String>();
        if displayed.len() < stderr.len() {
            displayed.push_str("\n…output truncated");
        }
        displayed
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
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn repository() -> PathBuf {
        let root = std::env::temp_dir().join(format!("waku-commit-test-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        run_git(&root, &["init", "-b", "main"]);
        run_git(&root, &["config", "user.name", "Waku Tests"]);
        run_git(&root, &["config", "user.email", "waku@example.com"]);
        fs::write(root.join("README.md"), "one\n").unwrap();
        run_git(&root, &["add", "."]);
        run_git(&root, &["commit", "-m", "initial"]);
        root
    }

    #[test]
    fn inspect_and_commit_include_unstaged_changes() {
        let root = repository();
        fs::write(root.join("README.md"), "one\ntwo\n").unwrap();
        let snapshot = inspect(&root).unwrap();
        assert_eq!(snapshot.branch, "main");
        assert!(snapshot.has_unstaged);
        assert_eq!(snapshot.additions, 1);

        commit(&root, "Update readme", true).unwrap();
        assert!(!inspect(&root).unwrap().has_unstaged);
        assert_eq!(
            git_stdout(&root, &["log", "-1", "--pretty=%s"]).unwrap(),
            "Update readme"
        );
    }

    #[test]
    fn staged_only_commit_leaves_later_worktree_edit() {
        let root = repository();
        fs::write(root.join("README.md"), "one\ntwo\n").unwrap();
        run_git(&root, &["add", "README.md"]);
        fs::write(root.join("README.md"), "one\ntwo\nthree\n").unwrap();

        commit(&root, "Stage one change", false).unwrap();
        let snapshot = inspect(&root).unwrap();
        assert!(!snapshot.has_staged);
        assert!(snapshot.has_unstaged);
    }

    #[test]
    fn staged_only_prompt_excludes_unstaged_edits() {
        let root = repository();
        fs::write(root.join("README.md"), "one\nstaged line\n").unwrap();
        run_git(&root, &["add", "README.md"]);
        fs::write(root.join("README.md"), "one\nstaged line\nunstaged line\n").unwrap();

        let staged_prompt = commit_prompt(&root, false).unwrap();
        assert!(staged_prompt.contains("staged line"));
        assert!(!staged_prompt.contains("unstaged line"));
        let all_prompt = commit_prompt(&root, true).unwrap();
        assert!(all_prompt.contains("unstaged line"));
    }

    #[test]
    fn normalizes_plain_or_wrapped_agent_output() {
        assert_eq!(
            normalize_message("\u{1b}[32m`Fix task UI.`\u{1b}[0m\n").as_deref(),
            Some("Fix task UI")
        );
        assert_eq!(
            normalize_message("Commit message: Add commit dialog\n").as_deref(),
            Some("Add commit dialog")
        );
    }

    fn has(args: &[OsString], value: &str) -> bool {
        args.iter().any(|arg| arg == value)
    }

    fn has_pair(args: &[OsString], first: &str, second: &str) -> bool {
        args.windows(2)
            .any(|pair| pair[0] == first && pair[1] == second)
    }

    #[test]
    fn every_provider_uses_a_noninteractive_generation_mode() {
        let prompt = "Generate subject";
        for provider in ProviderKind::ALL {
            let args = agent_arguments(
                provider,
                Some("model"),
                Some("low"),
                prompt,
                Some(Path::new("/tmp/amp.json")),
            );
            assert!(has(&args, prompt));
            match provider {
                ProviderKind::Amp => {
                    assert!(has(&args, "--execute"));
                    assert!(has_pair(&args, "--settings-file", "/tmp/amp.json"));
                    assert!(has_pair(&args, "--mode", "model"));
                    assert!(has_pair(&args, "--effort", "low"));
                }
                ProviderKind::Codex => {
                    assert_eq!(args.first().and_then(|arg| arg.to_str()), Some("exec"));
                    assert!(has_pair(&args, "--sandbox", "read-only"));
                    assert!(has(&args, "--ephemeral"));
                    assert!(has(&args, "--skip-git-repo-check"));
                }
                ProviderKind::OpenCode => {
                    assert_eq!(args.first().and_then(|arg| arg.to_str()), Some("run"));
                    assert!(has(&args, "--pure"));
                    assert!(has_pair(&args, "--agent", "plan"));
                    assert!(has_pair(&args, "--variant", "low"));
                }
                ProviderKind::Claude => {
                    assert!(has(&args, "--print"));
                    assert!(has_pair(&args, "--permission-mode", "plan"));
                    assert!(has_pair(&args, "--tools", ""));
                    assert!(has(&args, "--no-session-persistence"));
                }
                ProviderKind::Cursor => {
                    assert!(has(&args, "--print"));
                    assert!(has_pair(&args, "--mode", "ask"));
                    assert!(has_pair(&args, "--sandbox", "enabled"));
                }
                ProviderKind::DeepSeek => {
                    assert!(has_pair(&args, "--profile", "headless"));
                }
                ProviderKind::Fx => {
                    assert_eq!(args.first().and_then(|arg| arg.to_str()), Some("ask"));
                    assert!(has(&args, "--no-save"));
                    assert!(has(&args, "--no-color"));
                    assert!(has(&args, "--"));
                }
                ProviderKind::Grok => {
                    assert!(has_pair(&args, "--single", prompt));
                    assert!(has_pair(&args, "--permission-mode", "plan"));
                    assert!(has_pair(&args, "--tools", ""));
                    assert!(has(&args, "--no-memory"));
                    assert!(has(&args, "--no-subagents"));
                    assert!(has_pair(&args, "--reasoning-effort", "low"));
                }
                ProviderKind::Pi => {
                    assert!(has(&args, "--print"));
                    assert!(has(&args, "--no-session"));
                    assert!(has(&args, "--no-tools"));
                    assert!(has_pair(&args, "--thinking", "low"));
                }
                ProviderKind::OhMyPi => {
                    assert!(has(&args, "--print"));
                    assert!(has(&args, "--no-session"));
                    assert!(has(&args, "--no-tools"));
                    assert!(has(&args, "--no-rules"));
                    assert!(has_pair(&args, "--thinking", "low"));
                }
                ProviderKind::Kimi => {
                    assert!(has_pair(&args, "--prompt", prompt));
                    assert!(has_pair(&args, "--output-format", "text"));
                    assert!(has_pair(&args, "--model", "model"));
                }
            }
        }
    }

    /// The session's own model must not reach commit-message generation for the
    /// two providers that name a cheap tier: a subject line is the same job on
    /// Haiku as on Opus, and the diff is already in the prompt.
    #[test]
    fn claude_and_codex_generate_on_a_pinned_cheap_tier() {
        let claude = agent_arguments(
            ProviderKind::Claude,
            Some("claude-opus-5"),
            Some("xhigh"),
            "Generate subject",
            None,
        );
        assert!(has_pair(&claude, "--model", CLAUDE_COMMIT_MODEL));
        assert!(has_pair(&claude, "--effort", CLAUDE_COMMIT_EFFORT));
        assert!(!has(&claude, "claude-opus-5"));
        assert!(!has(&claude, "xhigh"));

        let codex = agent_arguments(
            ProviderKind::Codex,
            Some("gpt-5.6-sol"),
            Some("high"),
            "Generate subject",
            None,
        );
        assert!(has_pair(&codex, "--model", CODEX_COMMIT_MODEL));
        assert!(has_pair(&codex, "-c", CODEX_COMMIT_EFFORT));
        assert!(!has(&codex, "gpt-5.6-sol"));
        assert!(!has(&codex, "high"));
    }
}

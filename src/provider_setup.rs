//! Guided setup for provider CLIs owned by the local desktop host.
//!
//! Provider execution normally belongs to the daemon. Installation and
//! browser authentication are different: they mutate the signed-in user's
//! toolchain and open that user's browser, so Waku exposes them only while it
//! owns a local daemon. Each provider gets an explicit, reviewed recipe; an
//! unknown CLI is never installed by guessing a package name.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, bail};

use crate::model::ProviderKind;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProviderAuthState {
    Checking,
    SignedOut,
    SignedIn,
    Failed(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProviderSetupState {
    Installing,
    Authenticating,
    Failed(String),
}

#[derive(Debug)]
pub enum ProviderSetupEvent {
    Authenticating,
    OpenSetupGuide,
    Finished(anyhow::Result<()>),
}

pub fn setup_url(provider: ProviderKind) -> &'static str {
    match provider {
        ProviderKind::Amp => "https://ampcode.com/manual",
        ProviderKind::Claude => "https://code.claude.com/docs/en/setup",
        ProviderKind::Codex => "https://developers.openai.com/codex/cli",
        ProviderKind::Cursor => "https://docs.cursor.com/en/cli/installation",
        ProviderKind::DeepSeek => "https://github.com/deepseek-ai/deepseek-harness",
        ProviderKind::OpenCode => "https://opencode.ai/docs/providers",
        ProviderKind::Grok => "https://docs.x.ai/build/overview",
        ProviderKind::Pi => "https://github.com/earendil-works/pi/tree/main/packages/coding-agent",
    }
}

/// Providers with a non-interactive, vendor-owned installer Waku can safely
/// start on this host. Cursor publishes no native Windows CLI installer; its
/// documented Windows support is through WSL, whose binary cannot back Waku's
/// native daemon.
pub fn can_install_automatically(provider: ProviderKind) -> bool {
    !(cfg!(windows) && provider == ProviderKind::Cursor)
}

/// Browser-oriented sign-in commands that can be run without collecting a
/// credential or asking for secret input inside Waku.
pub fn can_authenticate_automatically(provider: ProviderKind) -> bool {
    matches!(
        provider,
        ProviderKind::Amp
            | ProviderKind::Claude
            | ProviderKind::Codex
            | ProviderKind::Cursor
            | ProviderKind::Grok
    )
}

/// CLIs with a stable, documented readiness command. Other providers still
/// expose Connect/Configure, but Waku does not infer authentication by reading
/// their credential stores.
pub fn supports_auth_probe(provider: ProviderKind) -> bool {
    matches!(
        provider,
        ProviderKind::Claude | ProviderKind::Codex | ProviderKind::Cursor | ProviderKind::Grok
    )
}

/// Check the provider's documented credential status without touching it.
pub fn probe_authentication(provider: ProviderKind, binary: &Path) -> anyhow::Result<bool> {
    let mut command = crate::command_env::command(binary);
    match provider {
        ProviderKind::Claude => command.args(["auth", "status"]),
        ProviderKind::Codex => command.args(["login", "status"]),
        ProviderKind::Cursor => command.arg("status"),
        // Grok Build has no separate status subcommand. `models` is the
        // provider-owned, read-only readiness check and explicitly reports the
        // signed-in account before returning its account catalog.
        ProviderKind::Grok => command.arg("models"),
        _ => bail!("authentication status is not available for this provider"),
    };
    let output = crate::command_env::output(&mut command)
        .with_context(|| format!("check {} authentication", provider.short_name()))?;
    if !output.status.success() {
        return Ok(false);
    }
    if provider == ProviderKind::Claude {
        let status: serde_json::Value =
            serde_json::from_slice(&output.stdout).context("read Claude authentication status")?;
        return Ok(status
            .get("loggedIn")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false));
    }
    if provider == ProviderKind::Cursor {
        let text = format!(
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
        .to_ascii_lowercase();
        return Ok(!text.contains("not authenticated") && !text.contains("not logged in"));
    }
    if provider == ProviderKind::Grok {
        let text = format!(
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return Ok(grok_models_report_signed_in(&text));
    }
    Ok(true)
}

fn grok_models_report_signed_in(output: &str) -> bool {
    let output = output.to_ascii_lowercase();
    output.contains("you are logged in")
        && output.contains("default model:")
        && output.contains("available models:")
}

/// Install the provider CLI into the current user's global npm prefix.
///
/// npm is deliberately resolved rather than invoked through a shell: the
/// package is static, no user input becomes code, and the child receives the
/// same augmented PATH as Waku's terminal and provider discovery.
pub fn install(provider: ProviderKind) -> anyhow::Result<PathBuf> {
    if !can_install_automatically(provider) {
        bail!(
            "{} requires its platform-specific install guide on this computer",
            provider.short_name()
        );
    }
    if provider == ProviderKind::Grok {
        install_grok()?;
        return find_installed_binary(provider);
    }
    if provider == ProviderKind::Cursor {
        install_cursor()?;
        return find_installed_binary(provider);
    }
    let (package, ignore_scripts) = match provider {
        ProviderKind::Amp => ("@ampcode/cli@latest", false),
        ProviderKind::Claude => ("@anthropic-ai/claude-code@latest", false),
        ProviderKind::Codex => ("@openai/codex@latest", false),
        ProviderKind::DeepSeek => ("@deepseek-ai/dsh@latest", false),
        ProviderKind::OpenCode => ("opencode-ai@latest", false),
        ProviderKind::Pi => ("@earendil-works/pi-coding-agent@latest", true),
        ProviderKind::Cursor | ProviderKind::Grok => unreachable!(),
    };
    let npm = crate::command_env::find_executable("npm").ok_or_else(|| {
        anyhow::anyhow!(
            "npm is required for automatic installation. Install Node.js, then retry, or open the setup guide"
        )
    })?;
    let mut command = crate::command_env::command(&npm);
    command.args(["install", "--global"]);
    if ignore_scripts {
        command.arg("--ignore-scripts");
    }
    command.arg(package);
    let output = crate::command_env::output(&mut command).context("start npm")?;
    if !output.status.success() {
        bail!(
            "{}",
            command_failure(
                &format!("npm could not install {}", provider.display_name()),
                &output
            )
        );
    }
    find_installed_binary(provider)
}

fn find_installed_binary(provider: ProviderKind) -> anyhow::Result<PathBuf> {
    crate::command_env::find_executable(provider.command()).ok_or_else(|| {
        anyhow::anyhow!(
            "{} was installed, but Waku could not find it. Refresh Providers or choose its binary path",
            provider.display_name()
        )
    })
}

#[cfg(windows)]
fn install_grok() -> anyhow::Result<()> {
    run_static_installer(
        "powershell.exe",
        &[
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            "irm https://x.ai/cli/install.ps1 | iex",
        ],
        "the Grok Build installer failed",
    )
}

#[cfg(not(windows))]
fn install_grok() -> anyhow::Result<()> {
    run_static_installer(
        "/bin/sh",
        &["-c", "curl -fsSL https://x.ai/cli/install.sh | sh"],
        "the Grok Build installer failed",
    )
}

#[cfg(not(windows))]
fn install_cursor() -> anyhow::Result<()> {
    run_static_installer(
        "/bin/sh",
        &["-c", "curl https://cursor.com/install -fsS | bash"],
        "the Cursor CLI installer failed",
    )
}

#[cfg(windows)]
fn install_cursor() -> anyhow::Result<()> {
    bail!("Cursor CLI supports Windows through WSL, not as a native Waku provider")
}

fn run_static_installer(program: &str, args: &[&str], failure: &str) -> anyhow::Result<()> {
    let mut command = crate::command_env::command(program);
    command.args(args);
    let output =
        crate::command_env::output(&mut command).with_context(|| format!("start {failure}"))?;
    if !output.status.success() {
        bail!("{}", command_failure(failure, &output));
    }
    Ok(())
}

/// Start the provider's browser sign-in and wait for the CLI to receive the
/// callback. The CLI owns credentials; Waku never reads or stores tokens.
pub fn authenticate(provider: ProviderKind, binary: &Path) -> anyhow::Result<()> {
    let mut command = crate::command_env::command(binary);
    match provider {
        ProviderKind::Amp | ProviderKind::Codex | ProviderKind::Cursor | ProviderKind::Grok => {
            command.arg("login");
        }
        ProviderKind::Claude => {
            command.args(["auth", "login"]);
        }
        _ => bail!("automatic authentication is not available for this provider"),
    }
    let output = crate::command_env::output(&mut command)
        .with_context(|| format!("start {} sign-in", provider.short_name()))?;
    if !output.status.success() {
        bail!(
            "{}",
            command_failure(
                &format!("{} sign-in did not finish", provider.short_name()),
                &output
            )
        );
    }
    if supports_auth_probe(provider) && !probe_authentication(provider, binary)? {
        bail!(
            "{} did not report an authenticated account after sign-in",
            provider.short_name()
        );
    }
    Ok(())
}

fn command_failure(prefix: &str, output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let detail = stderr
        .lines()
        .chain(stdout.lines())
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .last()
        .unwrap_or_default();
    let detail = truncate_error_detail(detail, 240);
    if detail.is_empty() {
        format!("{prefix} ({})", output.status)
    } else {
        format!("{prefix}: {detail}")
    }
}

fn truncate_error_detail(detail: &str, max_chars: usize) -> String {
    let mut chars = detail.chars();
    let truncated = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        format!("{truncated}…")
    } else {
        truncated
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guided_setup_is_opt_in_per_reviewed_provider() {
        assert!(
            ProviderKind::ALL
                .into_iter()
                .all(|provider| !setup_url(provider).is_empty())
        );
        assert!(supports_auth_probe(ProviderKind::Codex));
        assert!(supports_auth_probe(ProviderKind::Grok));
        assert!(!supports_auth_probe(ProviderKind::Pi));
        assert!(can_authenticate_automatically(ProviderKind::Claude));
        assert!(can_authenticate_automatically(ProviderKind::Grok));
        assert!(!can_authenticate_automatically(ProviderKind::OpenCode));
        assert_eq!(
            can_install_automatically(ProviderKind::Cursor),
            !cfg!(windows)
        );
        for provider in ProviderKind::ALL {
            if provider != ProviderKind::Cursor || !cfg!(windows) {
                assert!(can_install_automatically(provider));
            }
        }
    }

    #[test]
    fn setup_errors_stay_compact() {
        let detail = "x".repeat(300);
        let detail = truncate_error_detail(&detail, 240);
        assert_eq!(detail.chars().count(), 241);
        assert!(detail.ends_with('…'));
    }

    #[test]
    fn grok_model_catalog_is_a_read_only_auth_signal() {
        assert!(grok_models_report_signed_in(
            "You are logged in with grok.com.\nDefault model: grok-4.6\nAvailable models:\n* grok-4.6"
        ));
        assert!(!grok_models_report_signed_in("Authentication required"));
    }
}

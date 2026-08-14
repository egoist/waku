//! Daemon-only provider discovery layered over shared protocol models.

use std::path::Path;

pub use waku_protocol::model::*;

pub fn provider_probe(provider: ProviderKind, binary_override: Option<&str>) -> ProviderProbe {
    let path = match binary_override {
        Some(binary) => crate::command_env::resolve_binary_override(binary),
        None => crate::command_env::find_executable(provider.command()),
    };
    ProviderProbe {
        provider,
        installed: path.is_some(),
        path,
        models: crate::model_catalog::fallback_models(provider),
        agent_presets: crate::model_catalog::fallback_agent_presets(provider),
    }
}

pub fn discover_provider_models(mut probe: ProviderProbe) -> ProviderProbe {
    if probe.provider.supports_model_discovery()
        && let Some(path) = probe.path.as_deref()
    {
        let (models, agent_presets) = crate::model_catalog::discover_catalog(probe.provider, path);
        probe.models = models;
        probe.agent_presets = agent_presets;
    }
    probe
}

/// Run `<cli> --version` on the daemon host and extract its first version-like
/// token. Provider CLIs decorate this output differently, so clients receive a
/// normalized value rather than subprocess output.
pub fn probe_provider_version(binary: &Path) -> Option<String> {
    let mut command = crate::command_env::command(binary);
    let command = command.arg("--version").stdin(std::process::Stdio::null());
    let output = crate::command_env::output(command).ok()?;
    let combined = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    parse_cli_version(&combined)
}

//! Daemon-only provider discovery layered over shared protocol models.

use std::collections::BTreeMap;
use std::path::Path;

pub use waku_protocol::model::*;

pub fn provider_probe(provider: ProviderKind, binary_override: Option<&str>) -> ProviderProbe {
    provider_probe_for_instance(provider, None, binary_override)
}

pub fn provider_probe_for_instance(
    provider: ProviderKind,
    provider_instance_id: Option<&str>,
    binary_override: Option<&str>,
) -> ProviderProbe {
    let path = match binary_override {
        Some(binary) => crate::command_env::resolve_binary_override(binary),
        None => crate::command_env::find_executable(provider.command()),
    };
    ProviderProbe {
        provider,
        provider_instance_id: provider_instance_id
            .filter(|instance_id| *instance_id != provider.id())
            .map(str::to_owned),
        installed: path.is_some(),
        path,
        models: crate::model_catalog::fallback_models(provider),
        agent_presets: crate::model_catalog::fallback_agent_presets(provider),
    }
}

/// Detect a provider and hydrate its catalog from the daemon-owned cache.
///
/// This is the fast half of stale-while-revalidate: clients can render the
/// last successful catalog immediately, then request live discovery to replace
/// it. Cache I/O stays in the daemon instead of leaking host filesystem access
/// into desktop or Web clients.
pub fn cached_provider_probe(
    provider: ProviderKind,
    binary_override: Option<&str>,
) -> ProviderProbe {
    cached_provider_probe_for_instance(provider, None, binary_override)
}

pub fn cached_provider_probe_for_instance(
    provider: ProviderKind,
    provider_instance_id: Option<&str>,
    binary_override: Option<&str>,
) -> ProviderProbe {
    let identity = provider_instance_id.unwrap_or_else(|| provider.id());
    let cached = crate::model_catalog::cached_models_for_instance(provider, identity);
    apply_cached_models(
        provider_probe_for_instance(provider, provider_instance_id, binary_override),
        cached,
    )
}

fn apply_cached_models(
    mut probe: ProviderProbe,
    cached_models: Option<Vec<ProviderModel>>,
) -> ProviderProbe {
    if probe.provider.supports_model_discovery()
        && let Some(models) = cached_models
    {
        probe.models = models;
    }
    probe
}

pub fn discover_provider_models(probe: ProviderProbe) -> ProviderProbe {
    discover_provider_models_with_environment(probe, &BTreeMap::new())
}

pub fn discover_provider_models_with_environment(
    mut probe: ProviderProbe,
    environment: &BTreeMap<String, String>,
) -> ProviderProbe {
    if probe.provider.supports_model_discovery()
        && let Some(path) = probe.path.as_deref()
    {
        let (models, agent_presets) = crate::model_catalog::discover_catalog_for_instance(
            probe.provider,
            path,
            probe.provider_instance_id(),
            environment,
        );
        probe.models = models;
        probe.agent_presets = agent_presets;
    }
    probe
}

/// Run `<cli> --version` on the daemon host and extract its first version-like
/// token. Provider CLIs decorate this output differently, so clients receive a
/// normalized value rather than subprocess output.
pub fn probe_provider_version(binary: &Path) -> Option<String> {
    probe_provider_version_with_environment(binary, &BTreeMap::new())
}

pub fn probe_provider_version_with_environment(
    binary: &Path,
    environment: &BTreeMap<String, String>,
) -> Option<String> {
    let mut command = crate::command_env::command_with_environment(binary, environment);
    let command = command.arg("--version").stdin(std::process::Stdio::null());
    let output = crate::command_env::output(command).ok()?;
    let combined = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    parse_cli_version(&combined)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cached_catalog_replaces_fallback_before_live_discovery() {
        let probe = ProviderProbe {
            provider: ProviderKind::Codex,
            provider_instance_id: None,
            installed: true,
            path: Some("/usr/bin/codex".into()),
            models: crate::model_catalog::fallback_models(ProviderKind::Codex),
            agent_presets: Vec::new(),
        };
        let cached = vec![ProviderModel::new("cached-model", "Cached model").default()];

        let probe = apply_cached_models(probe, Some(cached));

        assert_eq!(probe.models.len(), 1);
        assert_eq!(probe.models[0].id, "cached-model");
    }
}

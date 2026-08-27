use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

use crate::computer_use::ComputerAppGrant;
use crate::model::ProviderKind;

fn provider_instance_enabled() -> bool {
    true
}

/// One concrete configuration of a supported coding-agent provider.
///
/// [`ProviderKind`] continues to select the protocol driver and its capabilities;
/// an instance supplies the launch details that may vary between installations or
/// accounts. Built-in instances are synthesized from the legacy provider settings,
/// while this shape is persisted for user-created instances.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ProviderInstance {
    /// Stable identity used by sessions, favorites, probe caches, and process pools.
    pub id: String,
    pub provider: ProviderKind,
    pub name: String,
    #[serde(default = "provider_instance_enabled")]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binary_override: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub environment: BTreeMap<String, String>,
}

impl ProviderInstance {
    pub fn builtin(provider: ProviderKind, enabled: bool, binary_override: Option<String>) -> Self {
        Self {
            id: provider.id().to_owned(),
            provider,
            name: provider.display_name().to_owned(),
            enabled,
            binary_override,
            environment: BTreeMap::new(),
        }
    }

    pub fn normalized(mut self) -> Option<Self> {
        self.id = self.id.trim().to_owned();
        self.name = self.name.trim().to_owned();
        self.binary_override = self
            .binary_override
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty());
        self.environment = self
            .environment
            .into_iter()
            .filter_map(|(key, value)| {
                let key = key.trim().to_owned();
                (!key.is_empty() && !key.contains('=')).then_some((key, value))
            })
            .collect();
        (!self.id.is_empty() && !self.name.is_empty()).then_some(self)
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, TS)]
#[serde(default)]
pub struct DaemonSettings {
    pub computer_use_enabled: bool,
    pub computer_use_allowed_apps: Vec<ComputerAppGrant>,
    pub disabled_providers: Vec<ProviderKind>,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub provider_binary_overrides: HashMap<ProviderKind, String>,
    /// User-created provider configurations. Built-in instances remain represented
    /// by `disabled_providers` and `provider_binary_overrides` for compatibility.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub custom_provider_instances: Vec<ProviderInstance>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl Default for DaemonSettings {
    fn default() -> Self {
        Self {
            computer_use_enabled: false,
            computer_use_allowed_apps: Vec::new(),
            disabled_providers: Vec::new(),
            provider_binary_overrides: HashMap::new(),
            custom_provider_instances: Vec::new(),
            extra: BTreeMap::new(),
        }
    }
}

impl DaemonSettings {
    pub fn default_path() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join(".waku")
            .join("settings.json")
    }

    pub fn discard_legacy_app_keys(&mut self) {
        for key in ["analytics_enabled", "favorite_models", "theme", "language"] {
            self.extra.remove(key);
        }
    }

    /// Returns the complete provider list shown by clients: built-ins first,
    /// followed by valid custom instances. Duplicate ids are ignored so one
    /// stable identity can never resolve to two launch configurations.
    pub fn provider_instances(&self) -> Vec<ProviderInstance> {
        let mut instances = ProviderKind::ALL
            .into_iter()
            .map(|provider| {
                ProviderInstance::builtin(
                    provider,
                    !self.disabled_providers.contains(&provider),
                    self.provider_binary_overrides.get(&provider).cloned(),
                )
            })
            .collect::<Vec<_>>();
        let mut ids = instances
            .iter()
            .map(|instance| instance.id.clone())
            .collect::<std::collections::HashSet<_>>();
        instances.extend(
            self.custom_provider_instances
                .iter()
                .cloned()
                .filter_map(ProviderInstance::normalized)
                .filter(|instance| ids.insert(instance.id.clone())),
        );
        instances
    }

    pub fn provider_instance(
        &self,
        provider: ProviderKind,
        instance_id: Option<&str>,
    ) -> Option<ProviderInstance> {
        let instance_id = instance_id.unwrap_or_else(|| provider.id());
        self.provider_instances()
            .into_iter()
            .find(|instance| instance.provider == provider && instance.id == instance_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_instances_keep_builtins_and_normalize_custom_launch_profiles() {
        let mut settings = DaemonSettings {
            disabled_providers: vec![ProviderKind::Codex],
            ..DaemonSettings::default()
        };
        settings.custom_provider_instances.push(ProviderInstance {
            id: " codex-work ".into(),
            provider: ProviderKind::Codex,
            name: " Work account ".into(),
            enabled: true,
            binary_override: Some(" codex ".into()),
            environment: BTreeMap::from([
                (" CODEX_HOME ".into(), "C:/codex/work".into()),
                ("INVALID=KEY".into(), "ignored".into()),
            ]),
        });
        // A custom row can never shadow a built-in instance identity.
        settings.custom_provider_instances.push(ProviderInstance {
            id: ProviderKind::Codex.id().into(),
            provider: ProviderKind::Codex,
            name: "shadow".into(),
            enabled: true,
            binary_override: None,
            environment: BTreeMap::new(),
        });

        let builtin = settings
            .provider_instance(ProviderKind::Codex, None)
            .expect("the built-in instance remains available");
        assert!(!builtin.enabled);
        assert!(builtin.environment.is_empty());

        let custom = settings
            .provider_instance(ProviderKind::Codex, Some("codex-work"))
            .expect("the custom instance resolves by stable id");
        assert_eq!(custom.name, "Work account");
        assert_eq!(custom.binary_override.as_deref(), Some("codex"));
        assert_eq!(
            custom.environment,
            BTreeMap::from([("CODEX_HOME".into(), "C:/codex/work".into())])
        );
        assert_eq!(
            settings
                .provider_instances()
                .iter()
                .filter(|instance| instance.provider == ProviderKind::Codex)
                .count(),
            2
        );
    }

    #[test]
    fn legacy_settings_deserialize_without_provider_instances() {
        let settings: DaemonSettings = serde_json::from_str(
            r#"{"disabled_providers":["codex"],"provider_binary_overrides":{"claude":"claude-custom"}}"#,
        )
        .unwrap();

        assert!(settings.custom_provider_instances.is_empty());
        assert!(
            !settings
                .provider_instance(ProviderKind::Codex, None)
                .unwrap()
                .enabled
        );
        assert_eq!(
            settings
                .provider_instance(ProviderKind::Claude, None)
                .unwrap()
                .binary_override
                .as_deref(),
            Some("claude-custom")
        );
    }
}

//! Skills that ship inside an agent CLI's plugins.
//!
//! A plugin vendors its own skills: the plugin directory holds a `skills/`
//! folder, and every skill in it is one the agent can load. Claude Code
//! installs a plugin under `<config>/plugins/cache/<marketplace>/<plugin>/<version>/`
//! and records the install in `plugins/installed_plugins.json`; Codex installs
//! one under `~/.codex/plugins/cache/<marketplace>/<plugin>/<version>/` and
//! records the plugins it has turned on in its own configuration.
//!
//! Those skills live in the plugin's own directory, never in the ecosystem's
//! skill root, so a scan of `~/.claude/skills` and `~/.codex/skills` never saw
//! them and the library page showed a plugin's skills as missing. This module
//! finds the plugin roots so they can be listed next to the skills a user
//! installed by hand.
//!
//! Each ecosystem is asked where its plugins are, in its own terms: a plugin
//! the user has turned off is left out, and a plugin installed for one
//! project is listed under that project.
//!
//! Everything here reads directories and files, so it runs on the background
//! executor only.

use std::cmp::Ordering;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::model::ProviderKind;
use waku_protocol::skills::{SkillLocation, SkillScope, SkillSource};

/// Upper bound on the plugins one scan reports; past this the library is
/// best-effort.
const MAX_PLUGINS: usize = 200;

/// Where an ecosystem installed a plugin, and what it calls it.
struct InstalledPlugin {
    /// The plugin's name, as the ecosystem spells it.
    name: String,
    /// The ecosystem's own id for the plugin, which is what its settings
    /// turn it off by.
    id: String,
    /// The plugin's own directory — the versioned directory it was installed
    /// into.
    root: PathBuf,
    /// The scope the install was recorded at.
    scope: PluginScope,
}

/// The scope an ecosystem recorded an install at.
enum PluginScope {
    /// Available everywhere the agent runs.
    User,
    /// Installed for one project, named by its path.
    Project(PathBuf),
}

/// Every skill root the installed plugins contribute, for the given projects.
///
/// A plugin's skills are only listed for the projects Waku knows about: a
/// project-scoped install belongs to a project the user has open.
pub fn plugin_skill_locations(projects: &[(String, PathBuf)]) -> Vec<SkillLocation> {
    let mut locations = Vec::new();
    locations.extend(claude_locations(projects));
    locations.extend(codex_locations(projects));
    locations
}

/// Claude Code's plugins. The ecosystem keeps its own list of what is
/// installed, and that list is the authority: the cache beside it keeps every
/// version a plugin was ever installed at, including the ones the user has
/// since removed.
fn claude_locations(projects: &[(String, PathBuf)]) -> Vec<SkillLocation> {
    let Some(config_dir) = claude_config_dir() else {
        return Vec::new();
    };
    let plugins_dir = config_dir.join("plugins");
    let plugins = match installed_plugins(&plugins_dir.join("installed_plugins.json")) {
        Some(plugins) => plugins,
        // Without a readable list, the cache is the only record of what was
        // installed.
        None => cached_plugins(&plugins_dir.join("cache")),
    };
    let disabled = disabled_plugins(&config_dir.join("settings.json"));
    let plugins = plugins
        .into_iter()
        .filter(|plugin| !is_disabled(&disabled, &plugin.id))
        .collect();
    locations_for(
        plugins,
        SkillSource::Provider(ProviderKind::Claude),
        projects,
    )
}

/// Codex's plugins. The ecosystem records the plugins it has turned on in its
/// own configuration, and installs each into its cache.
fn codex_locations(projects: &[(String, PathBuf)]) -> Vec<SkillLocation> {
    let Some(codex_home) = codex_home() else {
        return Vec::new();
    };
    let cache_root = codex_home.join("plugins/cache");
    let plugins = match enabled_plugins(&codex_home.join("config.toml")) {
        Some(enabled) => enabled
            .iter()
            .filter_map(|id| cached_plugin(&cache_root, id))
            .collect(),
        // Without a configuration that names the plugins, the cache is the
        // only record of what was installed.
        None => cached_plugins(&cache_root),
    };
    locations_for(
        plugins,
        SkillSource::Provider(ProviderKind::Codex),
        projects,
    )
}

/// The plugins an ecosystem has turned off in its settings.
fn disabled_plugins(settings_path: &Path) -> HashSet<String> {
    let Ok(contents) = std::fs::read_to_string(settings_path) else {
        return HashSet::new();
    };
    let Ok(settings) = serde_json::from_str::<serde_json::Value>(&contents) else {
        return HashSet::new();
    };
    let Some(enabled) = settings
        .get("enabledPlugins")
        .and_then(serde_json::Value::as_object)
    else {
        return HashSet::new();
    };
    enabled
        .iter()
        .filter(|(_, value)| value.as_bool() == Some(false))
        .map(|(id, _)| id.clone())
        .collect()
}

/// Whether an ecosystem has turned a plugin off. Its settings may name the
/// plugin by its full id or by its bare name.
fn is_disabled(disabled: &HashSet<String>, id: &str) -> bool {
    disabled.contains(id) || disabled.contains(&split_plugin_id(id).0)
}

/// The plugins an ecosystem has turned on, as its configuration records them.
///
/// `None` means the configuration named no plugins at all, so the caller has
/// to fall back to the install cache.
fn enabled_plugins(config_path: &Path) -> Option<Vec<String>> {
    let contents = std::fs::read_to_string(config_path).ok()?;
    let config = toml::from_str::<toml::Table>(&contents).ok()?;
    let plugins = config.get("plugins")?.as_table()?;
    Some(
        plugins
            .iter()
            .filter(|(_, value)| {
                value
                    .get("enabled")
                    .and_then(toml::Value::as_bool)
                    .unwrap_or(true)
            })
            .map(|(id, _)| id.clone())
            .collect(),
    )
}

/// Claude Code's configuration directory.
fn claude_config_dir() -> Option<PathBuf> {
    std::env::var("CLAUDE_CONFIG_DIR")
        .ok()
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| dirs::home_dir().map(|home| home.join(".claude")))
}

/// Codex's home directory.
fn codex_home() -> Option<PathBuf> {
    std::env::var("CODEX_HOME")
        .ok()
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| dirs::home_dir().map(|home| home.join(".codex")))
}

/// Turn installed plugins into the skill roots they contribute.
fn locations_for(
    plugins: Vec<InstalledPlugin>,
    source: SkillSource,
    projects: &[(String, PathBuf)],
) -> Vec<SkillLocation> {
    plugins
        .into_iter()
        .filter_map(|plugin| {
            let root = plugin_skills_root(&plugin.root)?;
            let (scope, project) = match &plugin.scope {
                PluginScope::User => (SkillScope::User, None),
                PluginScope::Project(path) => {
                    let (name, _) = projects
                        .iter()
                        .find(|(_, project_path)| same_path(project_path, path))?;
                    (SkillScope::Project, Some(name.clone()))
                }
            };
            Some(SkillLocation {
                source,
                scope,
                root,
                project,
                plugin: Some(plugin.name),
            })
        })
        .collect()
}

/// Whether two paths point at the same directory, however they are spelled.
fn same_path(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

/// The directory holding a plugin's skills.
///
/// A plugin's manifest may point its skills elsewhere — Codex's manifests
/// name the directory they ship — and the default is a `skills/` directory
/// beside the manifest.
fn plugin_skills_root(plugin_root: &Path) -> Option<PathBuf> {
    let manifest = read_manifest(plugin_root);
    let root = match manifest
        .as_ref()
        .and_then(|manifest| manifest.skills.as_deref())
    {
        Some(declared) => {
            let relative = declared
                .trim()
                .trim_start_matches("./")
                .trim_end_matches('/');
            if relative.is_empty() {
                return None;
            }
            plugin_root.join(relative)
        }
        None => plugin_root.join("skills"),
    };
    root.is_dir().then_some(root)
}

/// A plugin's manifest, in either ecosystem's spelling.
struct PluginManifest {
    name: Option<String>,
    /// The directory the manifest points its skills at.
    skills: Option<String>,
}

/// Read a plugin's manifest, preferring the ecosystem that wrote it.
fn read_manifest(plugin_root: &Path) -> Option<PluginManifest> {
    for relative in [".claude-plugin/plugin.json", ".codex-plugin/plugin.json"] {
        let Ok(contents) = std::fs::read_to_string(plugin_root.join(relative)) else {
            continue;
        };
        let Ok(manifest) = serde_json::from_str::<serde_json::Value>(&contents) else {
            continue;
        };
        let name = manifest
            .get("name")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_owned);
        let skills = manifest
            .get("skills")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|skills| !skills.is_empty())
            .map(str::to_owned);
        return Some(PluginManifest { name, skills });
    }
    None
}

/// The plugins an ecosystem recorded as installed.
///
/// Claude Code's list has two shapes: the current one maps a plugin to the
/// installs of that plugin, the older one maps it to a single install. Both
/// name the directory the plugin was installed into, which is all that is
/// needed here.
fn installed_plugins(path: &Path) -> Option<Vec<InstalledPlugin>> {
    let contents = std::fs::read_to_string(path).ok()?;
    let list = serde_json::from_str::<serde_json::Value>(&contents).ok()?;
    let plugins = list.get("plugins")?.as_object()?;
    let mut installed = Vec::new();
    for (id, entry) in plugins {
        let records = match entry {
            serde_json::Value::Array(records) => records.iter().collect::<Vec<_>>(),
            serde_json::Value::Object(_) => vec![entry],
            _ => continue,
        };
        for record in records {
            if installed.len() >= MAX_PLUGINS {
                return Some(installed);
            }
            let Some(root) = record
                .get("installPath")
                .and_then(serde_json::Value::as_str)
                .map(PathBuf::from)
                .filter(|root| root.is_dir())
            else {
                continue;
            };
            let scope = match record.get("scope").and_then(serde_json::Value::as_str) {
                Some("project") | Some("local") => match record
                    .get("projectPath")
                    .and_then(serde_json::Value::as_str)
                    .map(PathBuf::from)
                {
                    Some(path) => PluginScope::Project(path),
                    None => continue,
                },
                _ => PluginScope::User,
            };
            let fallback = plugin_name_from_id(id);
            let name = read_manifest(&root)
                .and_then(|manifest| manifest.name)
                .unwrap_or(fallback);
            installed.push(InstalledPlugin {
                name,
                id: id.to_owned(),
                root,
                scope,
            });
        }
    }
    Some(installed)
}

/// The plugins in a cache directory: `<marketplace>/<plugin>/<version>`.
///
/// The cache keeps every version a plugin was ever installed at, so only the
/// newest one is the plugin the user has.
fn cached_plugins(cache_root: &Path) -> Vec<InstalledPlugin> {
    let mut plugins = Vec::new();
    for marketplace in directories(cache_root) {
        let marketplace_name = file_name(&marketplace);
        for plugin in directories(&marketplace) {
            if plugins.len() >= MAX_PLUGINS {
                return plugins;
            }
            let Some(version) = newest_version(&plugin) else {
                continue;
            };
            let plugin_name = file_name(&plugin);
            let name = read_manifest(&version)
                .and_then(|manifest| manifest.name)
                .unwrap_or_else(|| plugin_name.clone());
            plugins.push(InstalledPlugin {
                name,
                id: format!("{plugin_name}@{marketplace_name}"),
                root: version,
                scope: PluginScope::User,
            });
        }
    }
    plugins
}

/// The installed copy of one plugin in a cache, named by the ecosystem's
/// plugin id.
fn cached_plugin(cache_root: &Path, id: &str) -> Option<InstalledPlugin> {
    let (name, marketplace) = split_plugin_id(id);
    let marketplace = marketplace?;
    let root = newest_version(&cache_root.join(&marketplace).join(&name))?;
    let display = read_manifest(&root)
        .and_then(|manifest| manifest.name)
        .unwrap_or_else(|| name.clone());
    Some(InstalledPlugin {
        name: display,
        id: id.to_owned(),
        root,
        scope: PluginScope::User,
    })
}

/// The newest version directory of a plugin, if it has one.
fn newest_version(plugin_root: &Path) -> Option<PathBuf> {
    directories(plugin_root)
        .into_iter()
        .max_by(|left, right| compare_versions(&file_name(left), &file_name(right)))
}

/// The directories inside a directory, in a stable order.
fn directories(path: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(path) else {
        return Vec::new();
    };
    let mut directories: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    directories.sort();
    directories
}

/// A directory's own name.
fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// The plugin's name, as the ecosystem's plugin id spells it: `name`, or
/// `name@marketplace`.
fn plugin_name_from_id(id: &str) -> String {
    split_plugin_id(id).0
}

/// Split an ecosystem's plugin id into its name and marketplace.
fn split_plugin_id(id: &str) -> (String, Option<String>) {
    let id = id.split("@^").next().unwrap_or(id);
    match id.split_once('@') {
        Some((name, marketplace)) => (name.to_owned(), Some(marketplace.to_owned())),
        None => (id.to_owned(), None),
    }
}

/// Compare two version strings: segment by segment, numerically where both
/// segments are numbers.
fn compare_versions(left: &str, right: &str) -> Ordering {
    let mut left = left.split('.');
    let mut right = right.split('.');
    loop {
        match (left.next(), right.next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(left), Some(right)) => {
                let order = match (left.parse::<u64>(), right.parse::<u64>()) {
                    (Ok(left), Ok(right)) => left.cmp(&right),
                    _ => left.cmp(right),
                };
                if order != Ordering::Equal {
                    return order;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(tag: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("waku-plugin-skills-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn write_skill(root: &Path, dir: &str) {
        std::fs::create_dir_all(root.join(dir)).unwrap();
        std::fs::write(
            root.join(dir).join("SKILL.md"),
            format!("---\nname: {dir}\n---\n"),
        )
        .unwrap();
    }

    fn write_manifest(root: &Path, relative: &str, contents: &str) {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    #[test]
    fn claude_plugins_come_from_the_installed_list() {
        let root = temp_root("claude-list");
        let plugin = root.join("cache/market/plugin/1.2.0");
        write_manifest(
            &plugin,
            ".claude-plugin/plugin.json",
            r#"{"name":"ship-it"}"#,
        );
        write_skill(&plugin, "skills/ship");
        std::fs::create_dir_all(root.join("plugins")).unwrap();
        std::fs::write(
            root.join("plugins/installed_plugins.json"),
            format!(
                r#"{{"version":2,"plugins":{{"plugin@market":[{{"scope":"user","installPath":"{}"}}]}}}}"#,
                plugin.display()
            ),
        )
        .unwrap();

        let plugins = installed_plugins(&root.join("plugins/installed_plugins.json")).unwrap();
        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0].name, "ship-it");
        assert!(matches!(plugins[0].scope, PluginScope::User));
        assert_eq!(
            plugin_skills_root(&plugins[0].root),
            Some(plugin.join("skills"))
        );
    }

    #[test]
    fn the_older_installed_list_shape_still_names_the_plugin() {
        let root = temp_root("claude-v1");
        let plugin = root.join("cache/market/plugin/1.0.0");
        write_skill(&plugin, "skills/old");
        let list = root.join("installed_plugins.json");
        std::fs::write(
            &list,
            format!(
                r#"{{"version":1,"plugins":{{"plugin@market":{{"version":"1.0.0","installPath":"{}"}}}}}}"#,
                plugin.display()
            ),
        )
        .unwrap();

        let plugins = installed_plugins(&list).unwrap();
        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0].name, "plugin");
        assert_eq!(plugins[0].root, plugin);
    }

    #[test]
    fn a_plugin_scoped_to_a_project_needs_that_project() {
        let root = temp_root("claude-project");
        let plugin = root.join("cache/market/plugin/1.0.0");
        write_skill(&plugin, "skills/scoped");
        let list = root.join("installed_plugins.json");
        std::fs::write(
            &list,
            format!(
                r#"{{"version":2,"plugins":{{"plugin@market":[{{"scope":"project","projectPath":"{}","installPath":"{}"}}]}}}}"#,
                root.join("work").display(),
                plugin.display()
            ),
        )
        .unwrap();

        let plugins = installed_plugins(&list).unwrap();
        let locations = locations_for(
            plugins,
            SkillSource::Provider(ProviderKind::Claude),
            &[("work".to_owned(), root.join("work"))],
        );
        assert_eq!(locations.len(), 1);
        assert_eq!(locations[0].scope, SkillScope::Project);
        assert_eq!(locations[0].project.as_deref(), Some("work"));
        assert_eq!(locations[0].plugin.as_deref(), Some("plugin"));

        // A project Waku does not know about has nowhere to list the skill.
        let locations = locations_for(
            installed_plugins(&list).unwrap(),
            SkillSource::Provider(ProviderKind::Claude),
            &[],
        );
        assert!(locations.is_empty());
    }

    #[test]
    fn the_cache_holds_the_newest_version_of_a_plugin() {
        let root = temp_root("cache");
        let old = root.join("market/plugin/1.9.0");
        let new = root.join("market/plugin/2.0.0");
        write_skill(&old, "skills/old");
        write_skill(&new, "skills/new");

        let plugins = cached_plugins(&root);
        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0].root, new);
        assert_eq!(plugins[0].name, "plugin");
    }

    #[test]
    fn a_manifest_can_point_its_skills_elsewhere() {
        let root = temp_root("manifest-skills");
        let plugin = root.join("market/plugin/1.0.0");
        write_manifest(
            &plugin,
            ".codex-plugin/plugin.json",
            r#"{"name":"custom","skills":"./bundled/skills/"}"#,
        );
        write_skill(&plugin, "bundled/skills/custom");

        let locations = locations_for(
            cached_plugins(&root),
            SkillSource::Provider(ProviderKind::Codex),
            &[],
        );
        assert_eq!(locations.len(), 1);
        assert_eq!(locations[0].root, plugin.join("bundled/skills"));
        assert_eq!(locations[0].plugin.as_deref(), Some("custom"));
    }

    #[test]
    fn versions_compare_by_segment() {
        assert_eq!(compare_versions("2.0.0", "1.9.0"), Ordering::Greater);
        assert_eq!(
            compare_versions("26.1117.31558", "1.0.109"),
            Ordering::Greater
        );
        assert_eq!(compare_versions("1.0.0", "1.0.0"), Ordering::Equal);
        assert_eq!(compare_versions("1.0", "1.0.1"), Ordering::Less);
    }

    #[test]
    fn a_plugin_the_user_turned_off_is_not_listed() {
        let root = temp_root("claude-disabled");
        let plugin = root.join("cache/market/plugin/1.0.0");
        write_skill(&plugin, "skills/ship");
        let list = root.join("installed_plugins.json");
        std::fs::write(
            &list,
            format!(
                r#"{{"version":2,"plugins":{{"plugin@market":[{{"scope":"user","installPath":"{}"}}]}}}}"#,
                plugin.display()
            ),
        )
        .unwrap();
        let settings = root.join("settings.json");
        std::fs::write(&settings, r#"{"enabledPlugins":{"plugin@market":false}}"#).unwrap();

        let disabled = disabled_plugins(&settings);
        let plugins = installed_plugins(&list).unwrap();
        let plugins: Vec<_> = plugins
            .into_iter()
            .filter(|plugin| !is_disabled(&disabled, &plugin.id))
            .collect();
        assert!(plugins.is_empty());

        // The settings may name the plugin by its bare name.
        let settings = root.join("bare.json");
        std::fs::write(&settings, r#"{"enabledPlugins":{"plugin":false}}"#).unwrap();
        let disabled = disabled_plugins(&settings);
        assert!(is_disabled(&disabled, "plugin@market"));
    }

    #[test]
    fn codex_names_the_plugins_it_has_turned_on() {
        let root = temp_root("codex-config");
        let config = root.join("config.toml");
        std::fs::write(
            &config,
            "[plugins.\"pdf@openai-primary-runtime\"]\nenabled = true\n",
        )
        .unwrap();
        assert_eq!(
            enabled_plugins(&config),
            Some(vec!["pdf@openai-primary-runtime".to_owned()])
        );

        let cache = root.join("plugins/cache/openai-primary-runtime/pdf/1.0.0");
        write_skill(&cache, "skills/pdf");
        let plugin =
            cached_plugin(&root.join("plugins/cache"), "pdf@openai-primary-runtime").unwrap();
        assert_eq!(plugin.root, cache);
        assert_eq!(plugin.name, "pdf");

        // A configuration that names no plugins leaves the caller to fall
        // back to the install cache.
        let plain = root.join("plain.toml");
        std::fs::write(&plain, "model = \"gpt\"\n").unwrap();
        assert_eq!(enabled_plugins(&plain), None);
    }
}

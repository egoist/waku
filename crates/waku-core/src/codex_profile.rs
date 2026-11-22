//! Codex config profiles.
//!
//! Codex keeps its user configuration in `CODEX_HOME` (by default
//! `~/.codex`). `config.toml` is the base configuration; every other
//! `<name>.config.toml` is a *profile* — a named layer Codex overlays on the
//! base when the user asks for it:
//!
//! ```text
//! codex --profile work            # runs with work.config.toml layered on config.toml
//! ```
//!
//! `codex app-server` — the transport Waku speaks — refuses `--profile`, and
//! the profile selector is gone from the config file itself, so a profile
//! reaches the app-server as the profile's own leaf values passed as
//! `-c key=value` overrides. That reproduces the layer Codex would have
//! built, minus the one ordering difference noted in
//! [`profile_overrides_in`].

use std::path::{Path, PathBuf};

/// The directory Codex keeps its configuration in.
pub fn codex_home() -> Option<PathBuf> {
    match std::env::var_os("CODEX_HOME") {
        Some(dir) if !dir.is_empty() => Some(PathBuf::from(dir)),
        _ => dirs::home_dir().map(|home| home.join(".codex")),
    }
}

/// The names of the profiles Codex can layer over the base configuration.
///
/// A profile is any `<name>.config.toml` beside the base `config.toml`.
/// Codex accepts letters, numbers, hyphens and underscores in a profile
/// name, so anything else is not a profile Waku should offer.
pub fn list_profiles() -> Vec<String> {
    let Some(home) = codex_home() else {
        return Vec::new();
    };
    list_profiles_in(&home)
}

/// The profiles in a given Codex home.
pub fn list_profiles_in(home: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(home) else {
        return Vec::new();
    };
    let mut names = Vec::new();
    for entry in entries.flatten() {
        let Ok(file_name) = entry.file_name().into_string() else {
            continue;
        };
        let Some(name) = file_name.strip_suffix(".config.toml") else {
            continue;
        };
        if !is_profile_name(name) {
            continue;
        }
        names.push(name.to_owned());
    }
    names.sort();
    names.dedup();
    names
}

/// The path of a profile's configuration file in a given Codex home.
pub fn profile_path_in(home: &Path, name: &str) -> Option<PathBuf> {
    if !is_profile_name(name) {
        return None;
    }
    Some(home.join(format!("{name}.config.toml")))
}

/// The profile's leaf values as `key=value` overrides for `codex -c`.
///
/// A profile that cannot be read or parsed yields no overrides rather than
/// an error: a broken profile file should not stop the session from starting
/// with the base configuration.
pub fn profile_overrides(name: &str) -> Vec<String> {
    let Some(home) = codex_home() else {
        return Vec::new();
    };
    profile_overrides_in(&home, name)
}

/// The profile's leaf values as `key=value` overrides for `codex -c`.
///
/// Codex loads the base configuration and then the profile on top of it, so a
/// profile only carries the values that differ. Flattening the profile into
/// overrides reproduces that result for every leaf Codex can express as a
/// dotted path — including tables such as `mcp_servers` and keys that
/// contain characters a dotted path would otherwise split, like a project's
/// path.
///
/// The one difference is precedence: Codex layers a profile *below* a
/// project's own configuration, while an override sits at the command line
/// and wins. A project that overrides a value the profile also sets keeps the
/// project's value under `codex --profile`, and takes the profile's value
/// here.
///
/// The overrides come back sorted by key, so the same profile always produces
/// the same command line.
pub fn profile_overrides_in(home: &Path, name: &str) -> Vec<String> {
    let Some(path) = profile_path_in(home, name) else {
        return Vec::new();
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    let Ok(value) = text.parse::<toml::Value>() else {
        return Vec::new();
    };
    let mut overrides = Vec::new();
    flatten("", &value, &mut overrides);
    overrides
        .into_iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect()
}

/// Whether a profile name is one Codex would accept.
fn is_profile_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Flatten a configuration table into dotted paths and their values.
fn flatten(prefix: &str, value: &toml::Value, out: &mut Vec<(String, String)>) {
    match value {
        toml::Value::Table(table) => {
            for (key, child) in table {
                let key = quote_key(key);
                let path = if prefix.is_empty() {
                    key
                } else {
                    format!("{prefix}.{key}")
                };
                flatten(&path, child, out);
            }
        }
        leaf => out.push((prefix.to_owned(), encode_value(leaf))),
    }
}

/// A key that is not a plain identifier has to be quoted, or the dotted path
/// would split it into pieces the configuration never had.
fn quote_key(key: &str) -> String {
    if !key.is_empty()
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return key.to_owned();
    }
    let mut quoted = String::with_capacity(key.len() + 2);
    quoted.push('"');
    for c in key.chars() {
        if c == '"' || c == '\\' {
            quoted.push('\\');
        }
        quoted.push(c);
    }
    quoted.push('"');
    quoted
}

/// Encode a leaf value the way `codex -c` expects it: as TOML, so a string
/// stays a string and a list stays a list.
fn encode_value(value: &toml::Value) -> String {
    value.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp_codex_home(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("waku-codex-profile-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create codex home");
        dir
    }

    fn write_profile(home: &Path, name: &str, contents: &str) {
        let mut file = std::fs::File::create(home.join(format!("{name}.config.toml")))
            .expect("create profile");
        file.write_all(contents.as_bytes()).expect("write profile");
    }

    #[test]
    fn profiles_are_the_config_files_beside_the_base_one() {
        let home = temp_codex_home("list");
        write_profile(&home, "work", "model = \"gpt-work\"\n");
        write_profile(&home, "deep-review", "model = \"gpt-review\"\n");
        std::fs::write(home.join("config.toml"), "model = \"gpt-base\"\n").unwrap();
        std::fs::write(home.join("notes.txt"), "not a profile").unwrap();
        std::fs::write(home.join("bad name.config.toml"), "model = \"x\"\n").unwrap();

        assert_eq!(
            list_profiles_in(&home),
            vec!["deep-review".to_owned(), "work".to_owned()]
        );
    }

    #[test]
    fn a_profile_becomes_dotted_overrides() {
        let home = temp_codex_home("overrides");
        write_profile(
            &home,
            "work",
            concat!(
                "model = \"gpt-work\"\n",
                "model_reasoning_effort = \"high\"\n",
                "\n",
                "[mcp_servers.docs]\n",
                "command = \"docs-server\"\n",
                "args = [\"--stdio\"]\n",
                "\n",
                "[projects.\"/tmp/example\"]\n",
                "trust_level = \"trusted\"\n",
            ),
        );

        assert_eq!(
            profile_overrides_in(&home, "work"),
            vec![
                "mcp_servers.docs.args=[\"--stdio\"]".to_owned(),
                "mcp_servers.docs.command=\"docs-server\"".to_owned(),
                "model=\"gpt-work\"".to_owned(),
                "model_reasoning_effort=\"high\"".to_owned(),
                "projects.\"/tmp/example\".trust_level=\"trusted\"".to_owned(),
            ]
        );
    }

    #[test]
    fn a_missing_or_broken_profile_yields_no_overrides() {
        let home = temp_codex_home("broken");
        write_profile(&home, "broken", "this is not toml = = =\n");

        assert!(profile_overrides_in(&home, "broken").is_empty());
        assert!(profile_overrides_in(&home, "absent").is_empty());
        assert!(profile_overrides_in(&home, "../escape").is_empty());
    }

    #[test]
    fn a_profile_name_may_not_escape_the_codex_home() {
        let home = temp_codex_home("escape");
        assert!(profile_path_in(&home, "../escape").is_none());
        assert!(profile_path_in(&home, "work").is_some());
        assert!(profile_path_in(&home, "work/../other").is_none());
    }
}

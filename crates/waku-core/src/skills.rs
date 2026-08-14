//! Agent-skill discovery and management for the Skills settings page.
//!
//! A skill is a directory holding a `SKILL.md` — reusable instructions any
//! coding agent can load. Every ecosystem keeps its own skill roots (see
//! [`crate::composer_complete::discover_slash_commands`] for the invocation
//! side); this module walks all of them at once so the settings page can show
//! one library across providers and projects.
//!
//! Installers routinely drop the same skill into several ecosystems' roots,
//! and dotfile setups symlink one directory everywhere. The catalog therefore
//! groups by name within a scope: one [`SkillEntry`] per skill, carrying
//! every [`SkillInstall`] it was found at, so the library lists each skill
//! once and mutations apply to all of its copies.
//!
//! Discovery reads directories and files, so it runs on the background
//! executor only. Mutations — create, enable, disable — are one-shot user
//! actions and may run synchronously in a click handler; each is a rename,
//! write, or mkdir per install.
//!
//! Disabling renames `SKILL.md` to `SKILL.md.disabled`. Every tool discovers
//! skills by that exact filename, so the rename hides the skill from all of
//! them at once while keeping the directory and its supporting files intact —
//! the same move people make by hand, made reversible with one toggle.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::model::ProviderKind;

pub use waku_protocol::skills::{
    DISABLED_SKILL_FILE, SKILL_FILE, SkillEntry, SkillInstall, SkillLocation, SkillScope,
    SkillSource, SkillsCatalog,
};

/// Upper bound on scanned skill directories; past this the library is
/// best-effort.
const SCAN_CAP: usize = 500;
const SKILL_FILE_MAX_BYTES: u64 = 256 * 1024;
/// Bounds for the per-skill supporting-file walk. Deep trees stop counting
/// rather than stall the scan; the sizes shown degrade to "at least this".
const DIR_WALK_MAX_DEPTH: usize = 6;
const DIR_WALK_MAX_FILES: usize = 500;

/// Every user-scope skill root, present on disk or not. Path joins only — no
/// filesystem access — so this is safe to call while building a frame.
pub fn user_skill_locations() -> Vec<SkillLocation> {
    let home = dirs::home_dir();
    let claude_config_dir = std::env::var("CLAUDE_CONFIG_DIR")
        .ok()
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| home.as_deref().map(|home| home.join(".claude")));
    let mut locations = Vec::new();
    let mut push = |source: SkillSource, root: Option<PathBuf>| {
        if let Some(root) = root {
            locations.push(SkillLocation {
                source,
                scope: SkillScope::User,
                root,
                project: None,
            });
        }
    };
    let home_join = |suffix: &str| home.as_deref().map(|home| home.join(suffix));
    push(SkillSource::Shared, home_join(".agents/skills"));
    push(
        SkillSource::Provider(ProviderKind::Claude),
        claude_config_dir.map(|dir| dir.join("skills")),
    );
    push(
        SkillSource::Provider(ProviderKind::Codex),
        home_join(".codex/skills"),
    );
    push(
        SkillSource::Provider(ProviderKind::OpenCode),
        home_join(".config/opencode/skills"),
    );
    push(
        SkillSource::Provider(ProviderKind::Cursor),
        home_join(".cursor/skills"),
    );
    push(
        SkillSource::Provider(ProviderKind::Pi),
        home_join(".pi/agent/skills"),
    );
    push(
        SkillSource::Provider(ProviderKind::Amp),
        home_join(".config/agents/skills"),
    );
    locations
}

/// Every project-scope skill root under `project_root`. Path joins only.
pub fn project_skill_locations(project_root: &Path, project_name: &str) -> Vec<SkillLocation> {
    [
        (SkillSource::Shared, ".agents/skills"),
        (
            SkillSource::Provider(ProviderKind::Claude),
            ".claude/skills",
        ),
        (SkillSource::Provider(ProviderKind::Codex), ".codex/skills"),
        (
            SkillSource::Provider(ProviderKind::OpenCode),
            ".opencode/skills",
        ),
        (
            SkillSource::Provider(ProviderKind::Cursor),
            ".cursor/skills",
        ),
        (SkillSource::Provider(ProviderKind::Pi), ".pi/skills"),
    ]
    .into_iter()
    .map(|(source, suffix)| SkillLocation {
        source,
        scope: SkillScope::Project,
        root: project_root.join(suffix),
        project: Some(project_name.to_owned()),
    })
    .collect()
}

/// All roots the scan walks for the given projects: user scope plus each
/// project's trees, in scan order.
pub fn skill_locations(projects: &[(String, PathBuf)]) -> Vec<SkillLocation> {
    let mut locations = user_skill_locations();
    for (name, path) in projects {
        locations.extend(project_skill_locations(path, name));
    }
    locations
}

/// One skill directory as found on disk, before grouping.
struct RawSkill {
    name: String,
    description: String,
    scope: SkillScope,
    project: Option<String>,
    install: SkillInstall,
    allowed_tools: Option<String>,
    body: String,
    supporting_files: usize,
    total_bytes: u64,
    modified_at: Option<u64>,
}

/// Walk every location and build the catalog. Filesystem work throughout —
/// background executor only.
pub fn scan_skills(locations: &[SkillLocation]) -> SkillsCatalog {
    let mut raw = Vec::new();
    for location in locations {
        scan_location(location, &mut raw);
        if raw.len() >= SCAN_CAP {
            break;
        }
    }

    // One entry per (scope group, name): the same skill installed into
    // several ecosystems' roots — copied or symlinked — is one skill.
    let mut skills: Vec<SkillEntry> = Vec::new();
    let mut by_identity: HashMap<(Option<String>, String), usize> = HashMap::new();
    for raw in raw {
        let key = (raw.project.clone(), raw.name.clone());
        match by_identity.get(&key) {
            Some(&index) => {
                let entry = &mut skills[index];
                entry.enabled |= raw.install.enabled;
                if entry.description.is_empty() {
                    entry.description = raw.description;
                }
                if entry.allowed_tools.is_none() {
                    entry.allowed_tools = raw.allowed_tools;
                }
                if entry.body.is_empty() {
                    entry.body = raw.body;
                }
                entry.modified_at = entry.modified_at.max(raw.modified_at);
                entry.installs.push(raw.install);
            }
            None => {
                by_identity.insert(key, skills.len());
                skills.push(SkillEntry {
                    name: raw.name,
                    description: raw.description,
                    scope: raw.scope,
                    project: raw.project,
                    enabled: raw.install.enabled,
                    installs: vec![raw.install],
                    allowed_tools: raw.allowed_tools,
                    body: raw.body,
                    supporting_files: raw.supporting_files,
                    total_bytes: raw.total_bytes,
                    modified_at: raw.modified_at,
                    duplicates: 0,
                    row_key: 0,
                });
            }
        }
    }

    skills.sort_by(|a, b| {
        let group = |skill: &SkillEntry| (skill.project.clone(), skill.name.to_lowercase());
        group(a).cmp(&group(b))
    });

    let mut name_counts: HashMap<&str, usize> = HashMap::new();
    for skill in &skills {
        *name_counts.entry(skill.name.as_str()).or_default() += 1;
    }
    let duplicates = skills
        .iter()
        .map(|skill| name_counts[skill.name.as_str()] - 1)
        .collect::<Vec<_>>();
    for (skill, duplicates) in skills.iter_mut().zip(duplicates) {
        skill.duplicates = duplicates;
        skill.row_key = {
            use std::hash::{Hash, Hasher};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            for install in &skill.installs {
                install.dir.hash(&mut hasher);
                install.enabled.hash(&mut hasher);
            }
            hasher.finish()
        };
    }
    SkillsCatalog { skills }
}

fn scan_location(location: &SkillLocation, raw: &mut Vec<RawSkill>) {
    let Ok(entries) = std::fs::read_dir(&location.root) else {
        return;
    };
    for entry in entries.flatten() {
        if raw.len() >= SCAN_CAP {
            return;
        }
        let dir = entry.path();
        // Follows symlinks — skills are routinely linked into place.
        if !dir.is_dir() {
            continue;
        }
        let Some(dir_name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if dir_name.starts_with('.') {
            continue;
        }
        let (skill_file, enabled) = if dir.join(SKILL_FILE).is_file() {
            (dir.join(SKILL_FILE), true)
        } else if dir.join(DISABLED_SKILL_FILE).is_file() {
            (dir.join(DISABLED_SKILL_FILE), false)
        } else {
            continue;
        };
        if std::fs::metadata(&skill_file).is_ok_and(|meta| meta.len() > SKILL_FILE_MAX_BYTES) {
            continue;
        }
        let Ok(contents) = std::fs::read_to_string(&skill_file) else {
            continue;
        };
        let front = parse_skill_frontmatter(&contents);
        let modified_at = std::fs::metadata(&skill_file)
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|duration| duration.as_secs());
        let (supporting_files, total_bytes) = measure_skill_dir(&dir);
        let mut body_end = front.body.len().min(BODY_PREVIEW_MAX_BYTES);
        while !front.body.is_char_boundary(body_end) {
            body_end -= 1;
        }
        raw.push(RawSkill {
            name: front.name.unwrap_or(dir_name),
            description: front.description.unwrap_or_default(),
            scope: location.scope,
            project: location.project.clone(),
            install: SkillInstall {
                source: location.source,
                dir,
                skill_file,
                enabled,
            },
            allowed_tools: front.allowed_tools,
            body: front.body[..body_end].trim().to_owned(),
            supporting_files,
            total_bytes,
            modified_at,
        });
    }
}

/// `(files beside the skill file, total bytes)` for a skill directory,
/// bounded so a runaway tree cannot stall the scan.
fn measure_skill_dir(dir: &Path) -> (usize, u64) {
    let mut files = 0usize;
    let mut bytes = 0u64;
    let mut stack = vec![(dir.to_path_buf(), 0usize)];
    while let Some((current, depth)) = stack.pop() {
        if depth > DIR_WALK_MAX_DEPTH || files >= DIR_WALK_MAX_FILES {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&current) else {
            continue;
        };
        for entry in entries.flatten() {
            if files >= DIR_WALK_MAX_FILES {
                break;
            }
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if name.starts_with('.') {
                continue;
            }
            // `fs::metadata` follows symlinks; the depth cap bounds cycles.
            match std::fs::metadata(entry.path()) {
                Ok(meta) if meta.is_dir() => stack.push((entry.path(), depth + 1)),
                Ok(meta) if meta.is_file() => {
                    files += 1;
                    bytes += meta.len();
                }
                _ => {}
            }
        }
    }
    // The skill file itself is counted in bytes but not listed as a
    // supporting file.
    (files.saturating_sub(1), bytes)
}

/// Bytes of a skill's document kept on the entry for the detail pane. The
/// full file stays one click away; the catalog only carries a render-ready
/// preview so a pathological library cannot pin megabytes.
const BODY_PREVIEW_MAX_BYTES: usize = 32 * 1024;

struct SkillFrontmatter<'a> {
    name: Option<String>,
    description: Option<String>,
    allowed_tools: Option<String>,
    /// The document below the frontmatter block.
    body: &'a str,
}

/// Pull the keys the page shows out of a leading YAML block, without a YAML
/// parser: skill files use flat `key: value` lines in practice, and an
/// unparsed extra key must not cost the skill its listing.
fn parse_skill_frontmatter(contents: &str) -> SkillFrontmatter<'_> {
    let mut front = SkillFrontmatter {
        name: None,
        description: None,
        allowed_tools: None,
        body: contents,
    };
    let Some(rest) = contents.strip_prefix("---") else {
        return front;
    };
    let Some((block, body)) = rest.split_once("\n---") else {
        return front;
    };
    front.body = body.trim_start_matches(['-']).trim_start();
    for line in block.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim().trim_matches('"').trim_matches('\'');
        if value.is_empty() {
            continue;
        }
        match key.trim() {
            "name" => front.name = Some(value.to_owned()),
            "description" => front.description = Some(value.to_owned()),
            "allowed-tools" => front.allowed_tools = Some(value.to_owned()),
            _ => {}
        }
    }
    front
}

/// Flip one copy's discoverability by renaming its skill file. Enabling a
/// copy that already has a live `SKILL.md` is a no-op rather than an error —
/// which also makes toggling a symlink-shared directory reached through
/// several installs idempotent.
pub fn set_skill_enabled(dir: &Path, enabled: bool) -> Result<(), String> {
    let live = dir.join(SKILL_FILE);
    let disabled = dir.join(DISABLED_SKILL_FILE);
    if enabled {
        if live.is_file() {
            return Ok(());
        }
        if !disabled.is_file() {
            return Ok(());
        }
        std::fs::rename(&disabled, &live).map_err(|error| error.to_string())
    } else {
        if !live.is_file() {
            return Ok(());
        }
        // `rename` replaces any stale `.disabled` leftover; the live file wins.
        std::fs::rename(&live, &disabled).map_err(|error| error.to_string())
    }
}

/// Move each concrete skill install to the daemon host's Trash.
pub fn trash_skills(dirs: &[PathBuf]) -> Result<(), String> {
    for dir in dirs {
        trash::delete(dir).map_err(|error| error.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("waku-skills-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn write_skill(root: &Path, dir: &str, contents: &str) {
        std::fs::create_dir_all(root.join(dir)).unwrap();
        std::fs::write(root.join(dir).join(SKILL_FILE), contents).unwrap();
    }

    fn user_location(source: SkillSource, root: &Path) -> SkillLocation {
        SkillLocation {
            source,
            scope: SkillScope::User,
            root: root.to_path_buf(),
            project: None,
        }
    }

    #[test]
    fn scan_reads_frontmatter_and_marks_disabled_entries() {
        let root = temp_root("scan");
        write_skill(
            &root,
            "deploy",
            "---\nname: deploy\ndescription: Ship it\nallowed-tools: Bash, Read\n---\nSteps…",
        );
        std::fs::create_dir_all(root.join("dormant")).unwrap();
        std::fs::write(
            root.join("dormant").join(DISABLED_SKILL_FILE),
            "---\nname: dormant\n---\nX",
        )
        .unwrap();
        // A directory without a skill file is not a skill.
        std::fs::create_dir_all(root.join("not-a-skill")).unwrap();
        // Supporting files count toward the entry's footprint.
        std::fs::write(root.join("deploy").join("runbook.md"), "details").unwrap();

        let locations = vec![user_location(SkillSource::Shared, &root)];
        let catalog = scan_skills(&locations);
        assert_eq!(catalog.skills.len(), 2);

        let deploy = catalog.skills.iter().find(|s| s.name == "deploy").unwrap();
        assert!(deploy.enabled);
        assert_eq!(deploy.description, "Ship it");
        assert_eq!(deploy.body, "Steps…");
        assert_eq!(deploy.allowed_tools.as_deref(), Some("Bash, Read"));
        assert_eq!(deploy.supporting_files, 1);
        assert!(deploy.total_bytes > 0);
        assert!(deploy.modified_at.is_some());
        assert_eq!(deploy.installs.len(), 1);
        assert_eq!(deploy.primary().source, SkillSource::Shared);

        let dormant = catalog.skills.iter().find(|s| s.name == "dormant").unwrap();
        assert!(!dormant.enabled);
        assert_eq!(catalog.disabled_count(), 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn copies_across_roots_group_into_one_entry() {
        let codex_root = temp_root("group-codex");
        let cursor_root = temp_root("group-cursor");
        let opencode_root = temp_root("group-opencode");
        // The installer-drops-it-everywhere layout: same skill, three roots.
        for root in [&codex_root, &cursor_root, &opencode_root] {
            write_skill(
                root,
                "agents-sdk",
                "---\nname: agents-sdk\ndescription: Build AI agents\n---\nX",
            );
        }
        let locations = vec![
            user_location(SkillSource::Provider(ProviderKind::Codex), &codex_root),
            user_location(SkillSource::Provider(ProviderKind::Cursor), &cursor_root),
            user_location(
                SkillSource::Provider(ProviderKind::OpenCode),
                &opencode_root,
            ),
        ];
        let catalog = scan_skills(&locations);
        assert_eq!(catalog.skills.len(), 1, "one row, not one per root");

        let skill = &catalog.skills[0];
        assert_eq!(skill.installs.len(), 3);
        assert_eq!(
            skill.primary().source,
            SkillSource::Provider(ProviderKind::Codex)
        );
        assert_eq!(skill.sources_label(), "Codex · Cursor · OpenCode");
        assert_eq!(skill.duplicates, 0, "grouped copies are not duplicates");

        // A disabled copy next to live ones keeps the skill enabled.
        set_skill_enabled(&cursor_root.join("agents-sdk"), false).unwrap();
        let catalog = scan_skills(&locations);
        assert!(catalog.skills[0].enabled);
        assert_eq!(catalog.skills[0].installs.len(), 3);

        for root in [&codex_root, &cursor_root, &opencode_root] {
            let _ = std::fs::remove_dir_all(root);
        }
    }

    #[test]
    fn scan_orders_user_before_projects_and_counts_cross_scope_duplicates() {
        let user_root = temp_root("order-user");
        let project_root = temp_root("order-project");
        write_skill(&user_root, "zeta", "---\nname: zeta\n---\nX");
        write_skill(&user_root, "review", "---\nname: review\n---\nX");
        write_skill(&project_root, "review", "---\nname: review\n---\nY");

        let locations = vec![
            user_location(SkillSource::Shared, &user_root),
            SkillLocation {
                source: SkillSource::Provider(ProviderKind::Claude),
                scope: SkillScope::Project,
                root: project_root.clone(),
                project: Some("waku".into()),
            },
        ];
        let catalog = scan_skills(&locations);
        let names: Vec<(&str, Option<&str>)> = catalog
            .skills
            .iter()
            .map(|skill| (skill.name.as_str(), skill.project.as_deref()))
            .collect();
        // User scope leads, alphabetical inside each group.
        assert_eq!(
            names,
            vec![("review", None), ("zeta", None), ("review", Some("waku"))]
        );
        // A user-scope and a project-scope copy stay separate rows — the
        // project one shadows at invocation — and both carry the note.
        assert!(
            catalog
                .skills
                .iter()
                .filter(|skill| skill.name == "review")
                .all(|skill| skill.duplicates == 1 && skill.installs.len() == 1)
        );
        let zeta = catalog.skills.iter().find(|s| s.name == "zeta").unwrap();
        assert_eq!(zeta.duplicates, 0);
        let _ = std::fs::remove_dir_all(&user_root);
        let _ = std::fs::remove_dir_all(&project_root);
    }

    #[test]
    fn enable_round_trips_and_never_destroys_a_live_skill_file() {
        let root = temp_root("toggle");
        write_skill(&root, "deploy", "---\nname: deploy\n---\nX");
        let dir = root.join("deploy");

        set_skill_enabled(&dir, false).unwrap();
        assert!(!dir.join(SKILL_FILE).exists());
        assert!(dir.join(DISABLED_SKILL_FILE).is_file());
        // Idempotent both ways.
        set_skill_enabled(&dir, false).unwrap();
        set_skill_enabled(&dir, true).unwrap();
        assert!(dir.join(SKILL_FILE).is_file());
        assert!(!dir.join(DISABLED_SKILL_FILE).exists());
        // Enabling with neither file present is a quiet no-op.
        set_skill_enabled(&root.join("missing"), true).unwrap();

        // A live SKILL.md next to a stale .disabled copy survives an enable.
        std::fs::write(dir.join(DISABLED_SKILL_FILE), "stale").unwrap();
        set_skill_enabled(&dir, true).unwrap();
        let contents = std::fs::read_to_string(dir.join(SKILL_FILE)).unwrap();
        assert!(contents.contains("name: deploy"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn every_ecosystem_root_is_listed() {
        let projects = vec![("waku".to_owned(), PathBuf::from("/tmp/waku"))];
        let locations = skill_locations(&projects);
        let roots: Vec<String> = locations
            .iter()
            .map(|location| location.root.display().to_string())
            .collect();
        for expected in [
            ".agents/skills",
            ".claude/skills",
            ".codex/skills",
            ".config/opencode/skills",
            ".cursor/skills",
            ".pi/agent/skills",
            ".config/agents/skills",
        ] {
            assert!(
                roots.iter().any(|root| root.ends_with(expected)),
                "user root missing: {expected}"
            );
        }
        for expected in [
            "/tmp/waku/.agents/skills",
            "/tmp/waku/.claude/skills",
            "/tmp/waku/.codex/skills",
            "/tmp/waku/.opencode/skills",
            "/tmp/waku/.cursor/skills",
            "/tmp/waku/.pi/skills",
        ] {
            assert!(
                roots.iter().any(|root| root == expected),
                "project root missing: {expected}"
            );
        }
        // User scope leads the scan, so grouped entries prefer user copies.
        assert!(locations[0].scope == SkillScope::User);
        assert_eq!(locations[0].source, SkillSource::Shared);
    }
}

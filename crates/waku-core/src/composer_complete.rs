//! Process-neutral composer autocompletion and daemon-side discovery.
//!
//! Everything here is plain data work — trigger parsing over the composer's
//! text, filesystem discovery of provider command files, fuzzy filtering — so
//! the popup itself can stay a pure view. Discovery walks directories and forks
//! `git`, which means it runs on the background executor only; the render path
//! reads the cached results and nothing else.

use std::collections::BTreeSet;
use std::ops::Range;
use std::path::{Path, PathBuf};

use crate::model::{ProviderKind, ReportedCommand};
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Matcher, Utf32Str};
pub use waku_protocol::composer::{CommandScope, FileEntry, SlashCommand};

/// How many rows a filter pass returns. The popup shows a screenful and the
/// keyboard walks the rest; past this the tail is noise, not choice.
pub const FILTER_CAP: usize = 64;

/// Upper bound on indexed workspace files. A generated monorepo can exceed any
/// budget; past this the index is best-effort rather than exhaustive.
pub const FILE_INDEX_CAP: usize = 50_000;

const COMMAND_SCAN_CAP: usize = 500;
const COMMAND_FILE_MAX_BYTES: u64 = 64 * 1024;
const WALK_MAX_DEPTH: usize = 8;

// ── Trigger detection ──────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TriggerKind {
    Command,
    File,
}

/// An autocompletion site under the caret: the token being typed, and the byte
/// range an accepted choice replaces (including the `/` or `@` sigil).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Trigger {
    pub kind: TriggerKind,
    pub query: String,
    pub range: Range<usize>,
}

/// The trigger at `cursor`, if the text under it is one.
///
/// A slash command must be the first token of its line — `/` mid-sentence is
/// prose. An `@` mention starts at any whitespace boundary, so `see @src/` in
/// the middle of a prompt still completes, while `user@host` does not: its `@`
/// is inside a token, not at the start of one.
pub fn detect_trigger(text: &str, cursor: usize) -> Option<Trigger> {
    let cursor = cursor.min(text.len());
    if !text.is_char_boundary(cursor) {
        return None;
    }
    let line_start = text[..cursor].rfind('\n').map_or(0, |index| index + 1);
    let line_prefix = &text[line_start..cursor];
    if let Some(query) = line_prefix.strip_prefix('/') {
        if !query.chars().any(char::is_whitespace) {
            return Some(Trigger {
                kind: TriggerKind::Command,
                query: query.to_owned(),
                range: line_start..cursor,
            });
        }
        return None;
    }

    let token_start = text[..cursor]
        .rfind(char::is_whitespace)
        .map_or(0, |index| {
            index + text[index..].chars().next().unwrap().len_utf8()
        });
    let token = &text[token_start..cursor];
    let query = token.strip_prefix('@')?;
    Some(Trigger {
        kind: TriggerKind::File,
        query: query.to_owned(),
        range: token_start..cursor,
    })
}

// ── Slash commands ─────────────────────────────────────────────────────────

/// Where a command came from, in the order groups are listed.
/// Waku's own built-ins. The shared commands are plain prompt templates Waku
/// expands at submit, so they work over any transport — unlike a CLI's native
/// built-ins, which only its own TUI understands. Provider-specific local
/// commands such as Codex's `/fast` are handled by the client without starting
/// a turn. A provider's native command of the same name is discovered first
/// and wins the collision.
fn builtin_waku_commands(provider: ProviderKind) -> Vec<SlashCommand> {
    let instructions_file = match provider {
        ProviderKind::Claude => "CLAUDE.md",
        _ => "AGENTS.md",
    };
    let mut commands = [
        (
            "init",
            tr!(
                "commands.init_description",
                instructions_file = instructions_file
            ),
            format!(
                "Analyze this repository and write {instructions_file} for coding agents \
                 working in it: build, test and lint commands, architecture overview, code \
                 conventions, and any gotchas. If the file already exists, update it and \
                 keep it concise."
            ),
        ),
        (
            "review",
            tr!("commands.review_description"),
            "Review the pending changes in this working tree: uncommitted work plus commits \
             not on the default branch. Look for bugs, regressions, and risky patterns; \
             report findings ordered by severity with file and line references."
                .to_owned(),
        ),
        (
            "commit",
            tr!("commands.commit_description"),
            "Look at the current working tree, stage the appropriate files, and create a git \
             commit with a clear message that describes the change and why it was made."
                .to_owned(),
        ),
    ]
    .into_iter()
    .map(|(name, description, template)| SlashCommand {
        name: name.to_owned(),
        description,
        scope: CommandScope::Builtin,
        argument_hint: None,
        template: Some(template),
    })
    .collect::<Vec<_>>();
    if provider == ProviderKind::Codex {
        commands.push(SlashCommand {
            name: "fast".to_owned(),
            description: tr!("commands.fast_description"),
            scope: CommandScope::Builtin,
            argument_hint: None,
            template: None,
        });
    }
    commands
}

/// Claude Code built-ins worth surfacing from a frontend. The live session's
/// init handshake later reports the authoritative list; this seeds the picker
/// before any turn has started.
fn builtin_claude_commands() -> Vec<SlashCommand> {
    const BUILTINS: [(&str, &str); 6] = [
        ("compact", "commands.claude_compact"),
        ("context", "commands.claude_context"),
        ("cost", "commands.claude_cost"),
        ("init", "commands.claude_init"),
        ("review", "commands.claude_review"),
        ("security-review", "commands.claude_security_review"),
    ];
    BUILTINS
        .into_iter()
        .map(|(name, description_key)| SlashCommand {
            name: name.to_owned(),
            description: crate::i18n::translate(description_key),
            scope: CommandScope::Builtin,
            argument_hint: None,
            template: None,
        })
        .collect()
}

/// Discover the slash commands available to `provider` inside `project_root`.
///
/// Filesystem work throughout — background executor only. Sources follow each
/// provider's own conventions:
///
/// - Claude Code: `.claude/commands` and `.claude/skills` in the project and
///   the config dir (`$CLAUDE_CONFIG_DIR`, default `~/.claude`), plus curated
///   built-ins. All passthrough: the CLI expands its own commands.
/// - Codex: `~/.codex/prompts`, expanded by Waku at submit.
/// - OpenCode: `.opencode/command` and `~/.config/opencode/command`, expanded
///   by Waku — its server transport takes plain prompt text.
/// - Cursor: `.cursor/commands` in the project and home, expanded by Waku.
/// - Pi: prompt templates in `.pi/prompts` and `~/.pi/agent/prompts`,
///   expanded by Waku, plus skills in `.pi/skills` and `~/.pi/agent/skills`.
/// - Amp registers commands through TypeScript plugins and Grok publishes no
///   file convention, so neither has a native command scan; Amp's skills in
///   `~/.config/agents/skills` are listed.
///
/// Skills — `SKILL.md` directories in each ecosystem's locations and the
/// cross-tool `.agents/skills` / `~/.agents/skills` — are listed for every
/// provider and always sent raw: `/skillname …` goes through verbatim and
/// the provider's own skill machinery takes it from there.
///
/// On top of the native sources, every provider reads Waku's own layer —
/// `.waku/commands` in the project and `~/.config/waku/commands` — and gets
/// Waku's built-ins ([`builtin_waku_commands`]), always expanded by Waku, so
/// a template written once works over any transport. Native files scan
/// first, so they win a same-scope name collision. Live processes may add
/// more at runtime ([`merge_reported_commands`]): Claude's init handshake
/// and ACP's `available_commands_update` for Cursor and Grok.
pub fn discover_slash_commands(provider: ProviderKind, project_root: &Path) -> Vec<SlashCommand> {
    let home = dirs::home_dir();
    let claude_config_dir = std::env::var("CLAUDE_CONFIG_DIR")
        .ok()
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| home.as_deref().map(|home| home.join(".claude")));
    let mut commands = Vec::new();
    match provider {
        ProviderKind::Claude => {
            scan_command_files(
                &project_root.join(".claude/commands"),
                CommandScope::Project,
                false,
                &mut commands,
            );
            if let Some(config_dir) = claude_config_dir.as_deref() {
                scan_command_files(
                    &config_dir.join("commands"),
                    CommandScope::User,
                    false,
                    &mut commands,
                );
            }
            scan_skill_files(&project_root.join(".claude/skills"), &mut commands);
            if let Some(config_dir) = claude_config_dir.as_deref() {
                scan_skill_files(&config_dir.join("skills"), &mut commands);
            }
            commands.extend(builtin_claude_commands());
        }
        ProviderKind::Codex => {
            scan_skill_files(&project_root.join(".codex/skills"), &mut commands);
            if let Some(home) = home.as_deref() {
                scan_command_files(
                    &home.join(".codex/prompts"),
                    CommandScope::User,
                    true,
                    &mut commands,
                );
                scan_skill_files(&home.join(".codex/skills"), &mut commands);
            }
        }
        ProviderKind::OpenCode => {
            scan_command_files(
                &project_root.join(".opencode/command"),
                CommandScope::Project,
                true,
                &mut commands,
            );
            scan_skill_files(&project_root.join(".opencode/skills"), &mut commands);
            // OpenCode also loads Claude-compatible skill trees.
            scan_skill_files(&project_root.join(".claude/skills"), &mut commands);
            if let Some(home) = home.as_deref() {
                scan_command_files(
                    &home.join(".config/opencode/command"),
                    CommandScope::User,
                    true,
                    &mut commands,
                );
                scan_skill_files(&home.join(".config/opencode/skills"), &mut commands);
                scan_skill_files(&home.join(".claude/skills"), &mut commands);
            }
        }
        ProviderKind::Cursor => {
            scan_command_files(
                &project_root.join(".cursor/commands"),
                CommandScope::Project,
                true,
                &mut commands,
            );
            scan_skill_files(&project_root.join(".cursor/skills"), &mut commands);
            if let Some(home) = home.as_deref() {
                scan_command_files(
                    &home.join(".cursor/commands"),
                    CommandScope::User,
                    true,
                    &mut commands,
                );
                scan_skill_files(&home.join(".cursor/skills"), &mut commands);
            }
        }
        ProviderKind::Pi => {
            scan_command_files(
                &project_root.join(".pi/prompts"),
                CommandScope::Project,
                true,
                &mut commands,
            );
            scan_skill_files(&project_root.join(".pi/skills"), &mut commands);
            if let Some(home) = home.as_deref() {
                scan_command_files(
                    &home.join(".pi/agent/prompts"),
                    CommandScope::User,
                    true,
                    &mut commands,
                );
                scan_skill_files(&home.join(".pi/agent/skills"), &mut commands);
            }
        }
        ProviderKind::Amp => {
            if let Some(home) = home.as_deref() {
                scan_skill_files(&home.join(".config/agents/skills"), &mut commands);
            }
        }
        // Harness commands are session-scoped and reported live by the Host.
        ProviderKind::DeepSeek | ProviderKind::Grok => {}
    }
    // The cross-tool skill standard, read by Amp and OpenCode among others;
    // Waku lists it for every provider.
    scan_skill_files(&project_root.join(".agents/skills"), &mut commands);
    if let Some(home) = home.as_deref() {
        scan_skill_files(&home.join(".agents/skills"), &mut commands);
    }
    scan_command_files(
        &project_root.join(".waku/commands"),
        CommandScope::Project,
        true,
        &mut commands,
    );
    if let Some(home) = home.as_deref() {
        scan_command_files(
            &home.join(".config/waku/commands"),
            CommandScope::User,
            true,
            &mut commands,
        );
    }
    commands.extend(builtin_waku_commands(provider));
    dedup_and_sort_commands(commands)
}

/// Fold the commands a live process reported into the discovered list.
///
/// Discovery cannot see plugin or dynamically registered commands; a report
/// cannot see templates and may lack descriptions. Discovered entries keep
/// their metadata — gaining a description when only the report has one — and
/// reported-only commands join as passthrough built-ins, since the process
/// that advertised them is the one that resolves them.
pub fn merge_reported_commands(
    discovered: &[SlashCommand],
    reported: &[ReportedCommand],
) -> Vec<SlashCommand> {
    let mut merged = discovered.to_vec();
    for report in reported {
        if let Some(known) = merged
            .iter_mut()
            .find(|command| command.name == report.name)
        {
            if known.description.is_empty() {
                known.description = report.description.clone();
            }
        } else {
            merged.push(SlashCommand {
                name: report.name.clone(),
                description: report.description.clone(),
                scope: CommandScope::Builtin,
                argument_hint: None,
                template: None,
            });
        }
    }
    sort_commands_for_display(&mut merged);
    merged
}

/// Most-specific scope wins on a name collision, matching how the CLIs resolve
/// their own lookups. Once collisions are resolved, the picker puts built-ins
/// first and skills last; within each scope the list reads alphabetically.
fn dedup_and_sort_commands(mut commands: Vec<SlashCommand>) -> Vec<SlashCommand> {
    // `CommandScope`'s declaration order describes resolution precedence,
    // which intentionally differs from the picker's presentation order.
    commands.sort_by(|a, b| (a.scope, &a.name).cmp(&(b.scope, &b.name)));
    let mut seen = BTreeSet::new();
    commands.retain(|command| seen.insert(command.name.clone()));
    sort_commands_for_display(&mut commands);
    commands
}

fn sort_commands_for_display(commands: &mut [SlashCommand]) {
    commands
        .sort_by(|a, b| (a.scope.display_rank(), &a.name).cmp(&(b.scope.display_rank(), &b.name)));
}

/// Collect `*.md` command files under `root`, one command per file, with
/// subdirectories namespacing the name (`a/b.md` → `a:b`).
fn scan_command_files(
    root: &Path,
    scope: CommandScope,
    expand: bool,
    commands: &mut Vec<SlashCommand>,
) {
    let mut stack = vec![(root.to_path_buf(), Vec::<String>::new())];
    while let Some((dir, namespace)) = stack.pop() {
        if namespace.len() > 3 || commands.len() >= COMMAND_SCAN_CAP {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if commands.len() >= COMMAND_SCAN_CAP {
                return;
            }
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if name.starts_with('.') {
                continue;
            }
            // `Path::is_dir` and `fs::metadata` follow symlinks, which
            // `DirEntry::file_type` does not — command and skill trees are
            // routinely symlinked out of dotfile repos.
            if path.is_dir() {
                let mut namespace = namespace.clone();
                namespace.push(name.to_owned());
                stack.push((path, namespace));
                continue;
            }
            let Some(stem) = name.strip_suffix(".md") else {
                continue;
            };
            if std::fs::metadata(&path).is_ok_and(|meta| meta.len() > COMMAND_FILE_MAX_BYTES) {
                continue;
            }
            let Ok(contents) = std::fs::read_to_string(&path) else {
                continue;
            };
            let front = parse_frontmatter(&contents);
            let name = namespace
                .iter()
                .map(String::as_str)
                .chain([stem])
                .collect::<Vec<_>>()
                .join(":");
            commands.push(SlashCommand {
                name,
                description: front
                    .description
                    .unwrap_or_else(|| first_line_summary(front.body)),
                scope,
                argument_hint: front.argument_hint,
                template: expand.then(|| front.body.trim().to_owned()),
            });
        }
    }
}

/// Collect skills — one directory per skill with a `SKILL.md` — as slash
/// commands. Always passthrough: the typed `/skillname …` goes to the
/// provider verbatim, and its own skill machinery takes it from there.
fn scan_skill_files(root: &Path, commands: &mut Vec<SlashCommand>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        if commands.len() >= COMMAND_SCAN_CAP {
            return;
        }
        // Follows symlinks — skills are routinely linked into place.
        if !entry.path().is_dir() {
            continue;
        }
        let Some(dir_name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if dir_name.starts_with('.') {
            continue;
        }
        let Ok(contents) = std::fs::read_to_string(entry.path().join("SKILL.md")) else {
            continue;
        };
        let front = parse_frontmatter(&contents);
        commands.push(SlashCommand {
            name: front.name.unwrap_or(dir_name),
            description: front.description.unwrap_or_default(),
            scope: CommandScope::Skill,
            argument_hint: None,
            template: None,
        });
    }
}

struct Frontmatter<'a> {
    name: Option<String>,
    description: Option<String>,
    argument_hint: Option<String>,
    body: &'a str,
}

/// Pull the few keys the picker shows out of a leading YAML block, without a
/// YAML parser: command files only use flat `key: value` lines in practice,
/// and an unparsed extra key must not cost the command its listing.
fn parse_frontmatter(contents: &str) -> Frontmatter<'_> {
    let mut front = Frontmatter {
        name: None,
        description: None,
        argument_hint: None,
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
            "argument-hint" => front.argument_hint = Some(value.to_owned()),
            _ => {}
        }
    }
    front
}

/// A command file with no description still deserves a one-line gloss: its
/// opening prose, clipped to a row's worth.
fn first_line_summary(body: &str) -> String {
    let line = body
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with('#'))
        .unwrap_or_default();
    let mut summary = String::with_capacity(line.len().min(100));
    for (count, character) in line.chars().enumerate() {
        if count >= 100 {
            summary.push('…');
            break;
        }
        summary.push(character);
    }
    summary
}

// ── Template expansion ─────────────────────────────────────────────────────

/// Substitute `$ARGUMENTS` and `$1`–`$9` — the dialect Claude Code and Codex
/// share — plus Pi's `$@`, in a command template. A template that consumes
/// nothing gets the arguments appended instead, so plain prompt files still
/// take input.
pub fn expand_command_template(template: &str, args: &str) -> String {
    let positional = args.split_whitespace().collect::<Vec<_>>();
    let mut expanded = String::with_capacity(template.len() + args.len());
    let mut consumed_args = false;
    let mut rest = template;
    while let Some(index) = rest.find('$') {
        expanded.push_str(&rest[..index]);
        let after = &rest[index + 1..];
        if let Some(tail) = after.strip_prefix("ARGUMENTS") {
            expanded.push_str(args);
            consumed_args = true;
            rest = tail;
        } else if let Some(tail) = after.strip_prefix('@') {
            expanded.push_str(args);
            consumed_args = true;
            rest = tail;
        } else if let Some(digit) = after
            .chars()
            .next()
            .and_then(|character| character.to_digit(10))
            .filter(|digit| (1..=9).contains(digit))
        {
            if let Some(argument) = positional.get(digit as usize - 1) {
                expanded.push_str(argument);
            }
            consumed_args = true;
            rest = &after[1..];
        } else {
            expanded.push('$');
            rest = after;
        }
    }
    expanded.push_str(rest);
    if !consumed_args && !args.is_empty() {
        expanded.push_str("\n\n");
        expanded.push_str(args);
    }
    expanded
}

/// The prompt to hand the driver for what the user typed.
///
/// `Some` only when the prompt invokes a known template command — the
/// transcript keeps the typed `/name …` either way, mirroring how the CLIs
/// echo the invocation rather than the expansion. Passthrough commands and
/// unknown `/words` go to the provider untouched.
pub fn expanded_submission(prompt: &str, commands: &[SlashCommand]) -> Option<String> {
    let invocation = prompt.strip_prefix('/')?;
    let (name, args) = match invocation.split_once(char::is_whitespace) {
        Some((name, args)) => (name, args.trim()),
        None => (invocation, ""),
    };
    let command = commands
        .iter()
        .find(|command| command.name == name && command.template.is_some())?;
    Some(expand_command_template(
        command.template.as_deref().unwrap_or_default(),
        args,
    ))
}

// ── Workspace file index ───────────────────────────────────────────────────

/// Index the project's files for `@` mentions. Background executor only.
///
/// `git ls-files` — tracked plus unignored untracked — is the authoritative
/// listing for a repository and costs one subprocess for the whole tree. A
/// plain directory gets a bounded walk instead.
pub fn list_project_files(root: &Path, cap: usize) -> Vec<FileEntry> {
    let files = git_listed_files(root, cap).unwrap_or_else(|| walked_files(root, cap));
    let mut directories = BTreeSet::new();
    for file in &files {
        let mut path = file.as_str();
        while let Some(index) = path.rfind('/') {
            path = &path[..index];
            if !directories.insert(path) {
                break;
            }
        }
    }
    let mut entries = directories
        .into_iter()
        .map(|directory| FileEntry {
            path: format!("{directory}/"),
            is_dir: true,
        })
        .chain(files.iter().map(|file| FileEntry {
            path: file.clone(),
            is_dir: false,
        }))
        .collect::<Vec<_>>();
    // Shallow entries first, so an empty query opens on the project's own
    // top level rather than deep generated paths.
    entries.sort_by(|a, b| {
        let depth = |entry: &FileEntry| entry.path.matches('/').count() - usize::from(entry.is_dir);
        (depth(a), &a.path).cmp(&(depth(b), &b.path))
    });
    entries.truncate(cap);
    entries
}

fn git_listed_files(root: &Path, cap: usize) -> Option<Vec<String>> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args([
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let listing = String::from_utf8(output.stdout).ok()?;
    Some(
        listing
            .split('\0')
            .filter(|path| !path.is_empty())
            .take(cap)
            .map(str::to_owned)
            .collect(),
    )
}

/// Directories a non-git walk never descends into: dependency and build output
/// trees drown the useful paths and can be arbitrarily large.
const WALK_SKIP: [&str; 7] = [
    "node_modules",
    "target",
    "dist",
    "build",
    "out",
    "vendor",
    "__pycache__",
];

fn walked_files(root: &Path, cap: usize) -> Vec<String> {
    let mut files = Vec::new();
    let mut stack = vec![(root.to_path_buf(), String::new(), 0usize)];
    while let Some((dir, prefix, depth)) = stack.pop() {
        if depth > WALK_MAX_DEPTH || files.len() >= cap {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if files.len() >= cap {
                break;
            }
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if name.starts_with('.') || WALK_SKIP.contains(&name.as_str()) {
                continue;
            }
            let relative = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            // `fs::metadata` follows symlinks; the depth cap bounds any
            // symlink cycle a walk could wander into.
            match std::fs::metadata(entry.path()) {
                Ok(meta) if meta.is_dir() => stack.push((entry.path(), relative, depth + 1)),
                Ok(meta) if meta.is_file() => files.push(relative),
                _ => {}
            }
        }
    }
    files
}

// ── Fuzzy filtering ────────────────────────────────────────────────────────

/// The matcher every filter call shares, so its internal scoring buffers are
/// allocated once for the window rather than once per keystroke.
pub fn matcher() -> Matcher {
    Matcher::new(nucleo_matcher::Config::DEFAULT.match_paths())
}

/// A filter survivor: the item plus the character positions the query matched,
/// sorted and deduplicated, so the popup can paint the matched characters.
/// Positions index characters of the matched text — the command name or the
/// full file path — not bytes.
#[derive(Clone, Debug)]
pub struct Scored<T> {
    pub item: T,
    pub positions: Vec<u32>,
}

/// Indexes of `haystack` entries matching `query` with their match positions,
/// best first, capped.
///
/// Two passes: scoring every candidate is cheap, while recovering match
/// positions walks the scoring matrix — so positions are computed only for
/// the rows that survive the cap. An empty query lists the head of the
/// haystack in its given order, so the popup opens with something to pick
/// before the first character narrows it.
fn filter_scored(
    haystack: &[&str],
    query: &str,
    matcher: &mut Matcher,
    cap: usize,
) -> Vec<(usize, Vec<u32>)> {
    if query.trim().is_empty() {
        return (0..haystack.len().min(cap))
            .map(|index| (index, Vec::new()))
            .collect();
    }
    let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
    let mut buf = Vec::new();
    let mut scored = Vec::new();
    for (index, text) in haystack.iter().enumerate() {
        if let Some(score) = pattern.score(Utf32Str::new(text, &mut buf), matcher) {
            scored.push((score, index));
        }
    }
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    scored.truncate(cap);
    scored
        .into_iter()
        .map(|(_, index)| {
            let mut positions = Vec::new();
            pattern.indices(
                Utf32Str::new(haystack[index], &mut buf),
                matcher,
                &mut positions,
            );
            positions.sort_unstable();
            positions.dedup();
            (index, positions)
        })
        .collect()
}

pub fn filter_commands(
    commands: &[SlashCommand],
    query: &str,
    matcher: &mut Matcher,
) -> Vec<Scored<SlashCommand>> {
    let names = commands
        .iter()
        .map(|command| command.name.as_str())
        .collect::<Vec<_>>();
    filter_scored(&names, query, matcher, FILTER_CAP)
        .into_iter()
        .map(|(index, positions)| Scored {
            item: commands[index].clone(),
            positions,
        })
        .collect()
}

pub fn filter_files(
    files: &[FileEntry],
    query: &str,
    matcher: &mut Matcher,
) -> Vec<Scored<FileEntry>> {
    let paths = files
        .iter()
        .map(|file| file.path.as_str())
        .collect::<Vec<_>>();
    filter_scored(&paths, query, matcher, FILTER_CAP)
        .into_iter()
        .map(|(index, positions)| Scored {
            item: files[index].clone(),
            positions,
        })
        .collect()
}

/// Byte ranges of `text` covered by the match `positions`, merged into
/// contiguous runs for painting.
///
/// `positions` index characters of the string `text` was sliced from;
/// `char_offset` is where `text` begins in it. A popup row renders one
/// matched string as several segments — a file row paints the basename and
/// its directory separately — and each segment recovers its own ranges this
/// way.
pub fn highlight_byte_ranges(
    text: &str,
    positions: &[u32],
    char_offset: usize,
) -> Vec<Range<usize>> {
    let mut ranges: Vec<Range<usize>> = Vec::new();
    for (char_index, (byte_index, character)) in (char_offset..).zip(text.char_indices()) {
        if positions.binary_search(&(char_index as u32)).is_ok() {
            let byte_end = byte_index + character.len_utf8();
            match ranges.last_mut() {
                Some(last) if last.end == byte_index => last.end = byte_end,
                _ => ranges.push(byte_index..byte_end),
            }
        }
    }
    ranges
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slash_triggers_only_as_the_first_token_of_a_line() {
        let trigger = detect_trigger("/com", 4).expect("slash at start triggers");
        assert_eq!(trigger.kind, TriggerKind::Command);
        assert_eq!(trigger.query, "com");
        assert_eq!(trigger.range, 0..4);

        assert!(detect_trigger("fix this /now", 13).is_none());
        assert!(detect_trigger("/compact the log", 16).is_none());
        // Mid-token cursor completes the typed half only.
        assert_eq!(detect_trigger("/compact", 4).unwrap().query, "com");
        // A slash on a later line is still a command position.
        let trigger = detect_trigger("first\n/rev", 10).expect("line start triggers");
        assert_eq!(trigger.range, 6..10);
    }

    #[test]
    fn at_triggers_on_token_start_only() {
        let trigger = detect_trigger("see @src/ap", 11).expect("token-start @ triggers");
        assert_eq!(trigger.kind, TriggerKind::File);
        assert_eq!(trigger.query, "src/ap");
        assert_eq!(trigger.range, 4..11);

        let trigger = detect_trigger("@", 1).expect("bare @ triggers");
        assert_eq!(trigger.query, "");
        assert!(detect_trigger("mail user@host", 14).is_none());
        assert!(detect_trigger("see @src done", 13).is_none());
        // Cursor before the sigil is not inside the token.
        assert!(detect_trigger("@src", 0).is_none());
    }

    #[test]
    fn frontmatter_yields_metadata_and_body() {
        let front = parse_frontmatter(
            "---\ndescription: \"Run the review\"\nargument-hint: [pr-number]\n---\nReview PR $1.",
        );
        assert_eq!(front.description.as_deref(), Some("Run the review"));
        assert_eq!(front.argument_hint.as_deref(), Some("[pr-number]"));
        assert_eq!(front.body, "Review PR $1.");

        let plain = parse_frontmatter("Just a prompt body.");
        assert!(plain.description.is_none());
        assert_eq!(plain.body, "Just a prompt body.");
        assert_eq!(
            first_line_summary("# Title\n\nThe real summary."),
            "The real summary."
        );
    }

    #[test]
    fn templates_substitute_positionals_and_arguments() {
        assert_eq!(
            expand_command_template("Review $1 against $2.", "src/a.rs main"),
            "Review src/a.rs against main."
        );
        assert_eq!(
            expand_command_template("Fix: $ARGUMENTS", "the login bug"),
            "Fix: the login bug"
        );
        // Pi templates spell the same thing `$@`.
        assert_eq!(
            expand_command_template("Fix: $@", "the bug"),
            "Fix: the bug"
        );
        // No placeholders: the arguments still arrive, appended.
        assert_eq!(
            expand_command_template("Run the linter.", "src only"),
            "Run the linter.\n\nsrc only"
        );
        // Every `$1`–`$9` is a placeholder — unfilled ones vanish, matching
        // the CLIs' own dialect — while non-positional dollar text survives.
        assert_eq!(
            expand_command_template("cost is $5 and $1", ""),
            "cost is  and "
        );
        assert_eq!(
            expand_command_template("literal $x or $0", ""),
            "literal $x or $0"
        );
    }

    #[test]
    fn submission_expands_template_commands_only() {
        let commands = vec![
            SlashCommand {
                name: "fix".into(),
                description: String::new(),
                scope: CommandScope::User,
                argument_hint: None,
                template: Some("Fix $ARGUMENTS carefully.".into()),
            },
            SlashCommand {
                name: "compact".into(),
                description: String::new(),
                scope: CommandScope::Builtin,
                argument_hint: None,
                template: None,
            },
        ];
        assert_eq!(
            expanded_submission("/fix the tests", &commands).as_deref(),
            Some("Fix the tests carefully.")
        );
        assert_eq!(expanded_submission("/compact", &commands), None);
        assert_eq!(expanded_submission("/unknown thing", &commands), None);
        assert_eq!(expanded_submission("plain prompt", &commands), None);
    }

    #[test]
    fn reported_commands_join_without_displacing_discovered_metadata() {
        let discovered = vec![
            SlashCommand {
                name: "compact".into(),
                description: "Free up context".into(),
                scope: CommandScope::Builtin,
                argument_hint: None,
                template: None,
            },
            SlashCommand {
                name: "bare".into(),
                description: String::new(),
                scope: CommandScope::Project,
                argument_hint: None,
                template: Some("body".into()),
            },
        ];
        let reported = vec![
            ReportedCommand {
                name: "compact".into(),
                description: "Provider copy".into(),
            },
            ReportedCommand {
                name: "bare".into(),
                description: "Filled in".into(),
            },
            ReportedCommand {
                name: "statusline".into(),
                description: String::new(),
            },
        ];
        let merged = merge_reported_commands(&discovered, &reported);
        assert_eq!(merged.len(), 3);
        assert_eq!(
            merged
                .iter()
                .map(|command| command.name.as_str())
                .collect::<Vec<_>>(),
            ["compact", "statusline", "bare"]
        );
        // Discovered metadata wins; a report only fills gaps.
        let compact = merged.iter().find(|c| c.name == "compact").unwrap();
        assert_eq!(compact.description, "Free up context");
        let bare = merged.iter().find(|c| c.name == "bare").unwrap();
        assert_eq!(bare.description, "Filled in");
        assert!(bare.template.is_some(), "templates must survive the merge");
        assert!(merged.iter().any(|c| c.name == "statusline"));
    }

    #[test]
    fn name_collisions_resolve_to_the_most_specific_scope() {
        let commands = dedup_and_sort_commands(vec![
            SlashCommand {
                name: "deploy".into(),
                description: "user copy".into(),
                scope: CommandScope::User,
                argument_hint: None,
                template: None,
            },
            SlashCommand {
                name: "deploy".into(),
                description: "project copy".into(),
                scope: CommandScope::Project,
                argument_hint: None,
                template: None,
            },
        ]);
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].description, "project copy");
    }

    #[test]
    fn command_picker_puts_builtins_first_and_skills_last() {
        let commands = dedup_and_sort_commands(vec![
            SlashCommand {
                name: "deploy".into(),
                description: String::new(),
                scope: CommandScope::Skill,
                argument_hint: None,
                template: None,
            },
            SlashCommand {
                name: "format".into(),
                description: String::new(),
                scope: CommandScope::User,
                argument_hint: None,
                template: None,
            },
            SlashCommand {
                name: "lint".into(),
                description: String::new(),
                scope: CommandScope::Project,
                argument_hint: None,
                template: None,
            },
            SlashCommand {
                name: "review".into(),
                description: String::new(),
                scope: CommandScope::Builtin,
                argument_hint: None,
                template: None,
            },
            SlashCommand {
                name: "commit".into(),
                description: String::new(),
                scope: CommandScope::Builtin,
                argument_hint: None,
                template: None,
            },
        ]);
        assert_eq!(
            commands
                .iter()
                .map(|command| (command.scope, command.name.as_str()))
                .collect::<Vec<_>>(),
            [
                (CommandScope::Builtin, "commit"),
                (CommandScope::Builtin, "review"),
                (CommandScope::Project, "lint"),
                (CommandScope::User, "format"),
                (CommandScope::Skill, "deploy"),
            ]
        );
    }

    #[test]
    fn file_filter_ranks_and_caps_matches() {
        let files = vec![
            FileEntry {
                path: "src/".into(),
                is_dir: true,
            },
            FileEntry {
                path: "src/app.rs".into(),
                is_dir: false,
            },
            FileEntry {
                path: "docs/appendix.md".into(),
                is_dir: false,
            },
            FileEntry {
                path: "README.md".into(),
                is_dir: false,
            },
        ];
        let mut matcher = matcher();
        let matched = filter_files(&files, "app", &mut matcher);
        let app = matched
            .iter()
            .find(|scored| scored.item.path == "src/app.rs")
            .expect("src/app.rs must match");
        // "app" matches contiguously at the basename: chars 4..7 of the path.
        assert_eq!(app.positions, vec![4, 5, 6]);
        assert!(matched.iter().all(|scored| scored.item.path != "README.md"));

        // Empty query keeps the given order, includes directories, and
        // carries no match positions to paint.
        let all = filter_files(&files, "", &mut matcher);
        assert_eq!(all[0].item.path, "src/");
        assert!(all[0].positions.is_empty());
        assert_eq!(all.len(), 4);
    }

    #[test]
    fn command_filter_matches_names() {
        let commands = builtin_claude_commands();
        let mut matcher = matcher();
        let matched = filter_commands(&commands, "sec", &mut matcher);
        assert_eq!(matched[0].item.name, "security-review");
        assert_eq!(matched[0].positions, vec![0, 1, 2]);
        assert_eq!(
            filter_commands(&commands, "", &mut matcher).len(),
            commands.len()
        );
    }

    #[test]
    fn highlight_ranges_merge_and_respect_segment_offsets() {
        // Positions over "src/app.rs": "a", "p", "p" at 4..7 and "r" at 8.
        let positions = vec![4, 5, 6, 8];
        // The basename segment starts at char 4.
        assert_eq!(
            highlight_byte_ranges("app.rs", &positions, 4),
            vec![0..3, 4..5]
        );
        // The directory segment sees none of them.
        assert_eq!(
            highlight_byte_ranges("src", &positions, 0),
            Vec::<Range<usize>>::new()
        );
        // Multi-byte characters produce byte-wide ranges.
        assert_eq!(highlight_byte_ranges("é.rs", &[0, 1], 0), vec![0..3]);
    }

    #[test]
    fn every_provider_offers_slash_commands_out_of_the_box() {
        let root = std::env::temp_dir().join(format!("waku-empty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        for provider in ProviderKind::ALL {
            let commands = discover_slash_commands(provider, &root);
            for expected in ["init", "review", "commit"] {
                let command = commands
                    .iter()
                    .find(|command| command.name == expected)
                    .unwrap_or_else(|| {
                        panic!("{} is missing /{expected}", provider.display_name())
                    });
                // Claude's own /init and /review pass through to the CLI;
                // everywhere else Waku expands its template.
                if provider != ProviderKind::Claude {
                    assert!(
                        command.template.is_some(),
                        "{} /{expected} must be a Waku template",
                        provider.display_name()
                    );
                }
            }
        }
        // The instructions file matches each ecosystem's convention.
        let claude = discover_slash_commands(ProviderKind::Claude, &root);
        let amp = discover_slash_commands(ProviderKind::Amp, &root);
        assert!(
            claude
                .iter()
                .any(|c| c.name == "commit" && c.template.is_some())
        );
        assert!(amp.iter().find(|c| c.name == "init").is_some_and(|c| {
            c.template
                .as_deref()
                .unwrap_or_default()
                .contains("AGENTS.md")
        }));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn fast_builtin_is_codex_only() {
        for provider in ProviderKind::ALL {
            let commands = builtin_waku_commands(provider);
            let fast = commands.iter().find(|command| command.name == "fast");
            if provider == ProviderKind::Codex {
                let fast = fast.expect("Codex is missing /fast");
                assert_eq!(fast.scope, CommandScope::Builtin);
                assert!(fast.template.is_none());
            } else {
                assert!(
                    fast.is_none(),
                    "{} unexpectedly offers /fast",
                    provider.display_name()
                );
            }
        }
    }

    #[test]
    fn shared_skills_are_listed_raw_on_every_provider() {
        let root = std::env::temp_dir().join(format!("waku-skills-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".agents/skills/deploy-runbook")).unwrap();
        std::fs::write(
            root.join(".agents/skills/deploy-runbook/SKILL.md"),
            "---\nname: deploy-runbook\ndescription: How we deploy\n---\nSteps…",
        )
        .unwrap();
        for provider in ProviderKind::ALL {
            let commands = discover_slash_commands(provider, &root);
            let skill = commands
                .iter()
                .find(|command| command.name == "deploy-runbook")
                .unwrap_or_else(|| panic!("{} misses the shared skill", provider.display_name()));
            assert_eq!(skill.scope, CommandScope::Skill);
            assert!(
                skill.template.is_none(),
                "skills are sent raw, never expanded"
            );
        }
        // Raw passthrough end to end: no expansion applies at submit.
        let commands = discover_slash_commands(ProviderKind::Amp, &root);
        assert_eq!(
            expanded_submission("/deploy-runbook staging", &commands),
            None
        );

        // Each ecosystem's own project-level skill tree is read too.
        for (provider, dir) in [
            (ProviderKind::Codex, ".codex/skills"),
            (ProviderKind::Cursor, ".cursor/skills"),
            (ProviderKind::OpenCode, ".opencode/skills"),
            (ProviderKind::Pi, ".pi/skills"),
        ] {
            let skill_dir = root.join(dir).join("native-skill");
            std::fs::create_dir_all(&skill_dir).unwrap();
            std::fs::write(
                skill_dir.join("SKILL.md"),
                "---\nname: native-skill\n---\nX",
            )
            .unwrap();
            assert!(
                discover_slash_commands(provider, &root)
                    .iter()
                    .any(|command| command.name == "native-skill"),
                "{} misses its project skill tree",
                provider.display_name()
            );
            let _ = std::fs::remove_dir_all(root.join(dir));
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn symlinked_skills_and_command_dirs_are_discovered() {
        let root = std::env::temp_dir().join(format!("waku-symlink-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        // The real content lives outside the scanned roots, linked in — the
        // dotfile-repo layout that a `DirEntry::file_type` check misses.
        std::fs::create_dir_all(root.join("real/my-skill")).unwrap();
        std::fs::write(
            root.join("real/my-skill/SKILL.md"),
            "---\nname: my-skill\ndescription: linked skill\n---\nBody",
        )
        .unwrap();
        std::fs::create_dir_all(root.join("real/commands")).unwrap();
        std::fs::write(root.join("real/commands/deploy.md"), "Ship it.").unwrap();
        std::fs::create_dir_all(root.join("skills")).unwrap();
        std::os::unix::fs::symlink(root.join("real/my-skill"), root.join("skills/my-skill"))
            .unwrap();
        std::os::unix::fs::symlink(root.join("real/commands"), root.join("commands")).unwrap();

        let mut commands = Vec::new();
        scan_skill_files(&root.join("skills"), &mut commands);
        assert!(
            commands.iter().any(|c| c.name == "my-skill"),
            "symlinked skill missing: {commands:?}"
        );
        scan_command_files(
            &root.join("commands"),
            CommandScope::User,
            true,
            &mut commands,
        );
        assert!(
            commands.iter().any(|c| c.name == "deploy"),
            "commands under a symlinked root missing: {commands:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn derived_directories_join_the_file_index() {
        let root = std::env::temp_dir().join(format!("waku-files-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src/deep")).unwrap();
        std::fs::write(root.join("src/deep/lib.rs"), "x").unwrap();
        std::fs::write(root.join("main.rs"), "x").unwrap();
        let entries = list_project_files(&root, 100);
        let paths = entries
            .iter()
            .map(|entry| entry.path.as_str())
            .collect::<Vec<_>>();
        assert!(paths.contains(&"src/"), "derived dir missing: {paths:?}");
        assert!(paths.contains(&"src/deep/lib.rs"));
        // Shallow entries lead.
        assert_eq!(paths[0], "main.rs");
        assert!(
            paths.iter().position(|p| *p == "main.rs").unwrap()
                < paths.iter().position(|p| *p == "src/deep/lib.rs").unwrap()
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}

//! Immutable, render-ready Git diffs for Waku's Review surface.
//!
//! Every function in this module performs subprocess work or parses potentially
//! large output. Callers run [`collect`] on the background executor and keep the
//! resulting snapshot in memory; a frame only indexes its visible rows.

use std::collections::HashMap;
use std::path::Path;
use std::process::{Command, Output};

use anyhow::{Context as _, anyhow, bail};
use unicode_width::UnicodeWidthChar;
use uuid::Uuid;

use crate::checkpoint;
use crate::git_branch;
use crate::md::highlight::{Carry, Lang, Token, lang_for_tag, tokenize_line};

const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";
const COLLAPSED_CONTEXT_LINES: usize = 3;
const COLLAPSED_CONTEXT_THRESHOLD: usize = 1;
/// Pierre expands a directional hunk control in 100-line increments. The
/// count label itself expands the complete region.
pub const DEFAULT_EXPANSION_LINE_COUNT: usize = 100;
/// Full-file context is what makes collapsed regions expandable without any
/// frame-time I/O. Keep an escape hatch for pathological generated files; the
/// compact patch remains reviewable when hydrating all context would retain an
/// unreasonable amount of text.
const MAX_HYDRATED_PATCH_BYTES: usize = 32 * 1024 * 1024;
/// A pathological generated patch must not turn one Review tab into an
/// unbounded in-memory document. The complete file summary remains available
/// in the tree when rendered lines are capped.
const MAX_RENDERED_DIFF_LINES: usize = 50_000;

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum Source {
    LastTurn {
        session_id: Uuid,
        turn_id: Uuid,
        turn_count: usize,
    },
    #[default]
    Uncommitted,
    Unstaged,
    Staged,
    Committed,
    Branch,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FileStatus {
    Added,
    Modified,
    Deleted,
    Binary,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct File {
    pub path: String,
    pub additions: u64,
    pub deletions: u64,
    pub status: FileStatus,
    /// First line of this file in [`Snapshot::lines`]. `None` means the patch
    /// was beyond the safety cap or Git emitted no textual body.
    pub diff_line: Option<usize>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GapPosition {
    Leading,
    Between,
    Trailing,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExpansionDirection {
    Start,
    End,
    Both,
    All,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Gap {
    /// Stable across incremental expansion so focus handles do not churn.
    pub id: u64,
    /// The number displayed when full context could not be retained.
    count: u32,
    /// Still-hidden context in file order. An empty vector with a non-zero
    /// count is a deliberately non-expandable compact-patch fallback.
    hidden: Vec<Line>,
    pub position: GapPosition,
}

impl Gap {
    pub fn count(&self) -> u32 {
        self.count
    }

    pub fn is_expandable(&self) -> bool {
        self.count > 0 && self.hidden.len() == self.count as usize
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum LineKind {
    FileHeader,
    /// Context collapsed between or around changed regions.
    Gap(Gap),
    HunkHeader,
    Context,
    Addition,
    Deletion,
    Meta,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub file_index: usize,
    pub old_line: Option<u32>,
    pub new_line: Option<u32>,
    pub kind: LineKind,
    /// Code content without the unified-diff marker for code rows; raw Git
    /// metadata for hunk/meta rows.
    pub content: String,
    /// Paint-only syntax spans over `content`, computed off the UI thread.
    pub tokens: Vec<Token>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    pub source: Source,
    pub files: Vec<File>,
    pub lines: Vec<Line>,
    /// Widest source line in monospace columns, counting the context retained
    /// inside collapsed gaps so expanding a gap never changes the horizontal
    /// extent, and skipping rows the visible-line cap dropped so the extent
    /// never exceeds what the list can show.
    pub max_content_columns: usize,
    pub additions: u64,
    pub deletions: u64,
    pub truncated: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GapExpansion {
    /// Number of rows replacing the separator row in the virtualized list.
    pub replacement_count: usize,
}

impl Snapshot {
    /// Reveal retained context without touching Git or the filesystem. The
    /// returned replacement count feeds `ListState::splice`, which keeps the
    /// viewport anchored while the separator turns into ordinary code rows.
    pub fn expand_gap(
        &mut self,
        line_index: usize,
        direction: ExpansionDirection,
    ) -> Option<GapExpansion> {
        let line = self.lines.get(line_index)?.clone();
        let LineKind::Gap(mut gap) = line.kind else {
            return None;
        };
        if !gap.is_expandable() {
            return None;
        }

        let visible_without_gap = self.lines.len().saturating_sub(1);
        let available = MAX_RENDERED_DIFF_LINES.saturating_sub(visible_without_gap);
        if available == 0 {
            self.truncated = true;
            return None;
        }

        let requested = match direction {
            ExpansionDirection::Start | ExpansionDirection::End => DEFAULT_EXPANSION_LINE_COUNT,
            ExpansionDirection::Both => DEFAULT_EXPANSION_LINE_COUNT.saturating_mul(2),
            ExpansionDirection::All => gap.hidden.len(),
        };
        let reveal_count = requested.min(gap.hidden.len()).min(available);
        if reveal_count == 0 {
            return None;
        }
        if direction == ExpansionDirection::All && reveal_count < gap.hidden.len() {
            self.truncated = true;
        }

        let mut replacement = Vec::with_capacity(reveal_count + 1);
        match direction {
            ExpansionDirection::Start => {
                replacement.extend(gap.hidden.drain(..reveal_count));
                push_remaining_gap(&mut replacement, line.file_index, gap);
            }
            ExpansionDirection::End => {
                let revealed = gap.hidden.split_off(gap.hidden.len() - reveal_count);
                push_remaining_gap(&mut replacement, line.file_index, gap);
                replacement.extend(revealed);
            }
            ExpansionDirection::Both if reveal_count == gap.hidden.len() => {
                replacement.extend(gap.hidden);
            }
            ExpansionDirection::Both => {
                let from_start = reveal_count.div_ceil(2);
                let from_end = reveal_count - from_start;
                replacement.extend(gap.hidden.drain(..from_start));
                let revealed_end = gap.hidden.split_off(gap.hidden.len() - from_end);
                push_remaining_gap(&mut replacement, line.file_index, gap);
                replacement.extend(revealed_end);
            }
            ExpansionDirection::All if reveal_count == gap.hidden.len() => {
                replacement.extend(gap.hidden);
            }
            ExpansionDirection::All => match gap.position {
                GapPosition::Leading => {
                    let revealed = gap.hidden.split_off(gap.hidden.len() - reveal_count);
                    push_remaining_gap(&mut replacement, line.file_index, gap);
                    replacement.extend(revealed);
                }
                GapPosition::Trailing => {
                    replacement.extend(gap.hidden.drain(..reveal_count));
                    push_remaining_gap(&mut replacement, line.file_index, gap);
                }
                GapPosition::Between => {
                    let from_start = reveal_count.div_ceil(2);
                    let from_end = reveal_count - from_start;
                    replacement.extend(gap.hidden.drain(..from_start));
                    let revealed_end = if from_end == 0 {
                        Vec::new()
                    } else {
                        gap.hidden.split_off(gap.hidden.len() - from_end)
                    };
                    push_remaining_gap(&mut replacement, line.file_index, gap);
                    replacement.extend(revealed_end);
                }
            },
        }

        let replacement_count = replacement.len();
        self.lines.splice(line_index..line_index + 1, replacement);
        let inserted = replacement_count.saturating_sub(1);
        if inserted > 0 {
            for file in &mut self.files {
                if let Some(diff_line) = file.diff_line.as_mut()
                    && *diff_line > line_index
                {
                    *diff_line = diff_line.saturating_add(inserted);
                }
            }
        }
        Some(GapExpansion { replacement_count })
    }
}

fn push_remaining_gap(replacement: &mut Vec<Line>, file_index: usize, mut gap: Gap) {
    if gap.hidden.is_empty() {
        return;
    }
    gap.count = gap.hidden.len() as u32;
    replacement.push(Line {
        file_index,
        old_line: None,
        new_line: None,
        kind: LineKind::Gap(gap),
        content: String::new(),
        tokens: Vec::new(),
    });
}

#[derive(Clone, Debug)]
struct Range {
    from: String,
    to: String,
}

/// Capture one consistent source pair and parse its unified diff.
pub fn collect(cwd: &Path, source: Source) -> anyhow::Result<Snapshot> {
    ensure_repository(cwd)?;
    let range = resolve_range(cwd, source)?;
    let numstat = diff_output(cwd, &range, &["--numstat"])?;
    let hydrated_patch = diff_output(cwd, &range, &["--unified=2147483647"])?;
    if hydrated_patch.len() <= MAX_HYDRATED_PATCH_BYTES {
        Ok(parse(source, &numstat, &hydrated_patch, true))
    } else {
        let compact_patch = diff_output(cwd, &range, &["--unified=3"])?;
        let mut snapshot = parse(source, &numstat, &compact_patch, false);
        snapshot.truncated = true;
        Ok(snapshot)
    }
}

fn resolve_range(cwd: &Path, source: Source) -> anyhow::Result<Range> {
    let head = resolve(cwd, "HEAD").unwrap_or_else(|| EMPTY_TREE.to_owned());
    Ok(match source {
        Source::LastTurn {
            session_id,
            turn_count,
            ..
        } => {
            if turn_count == 0 {
                bail!("the first checkpoint is a baseline, not a completed turn");
            }
            let diff_base_ref = checkpoint::turn_diff_base_ref(session_id, turn_count);
            let start_ref = checkpoint::turn_start_ref(session_id, turn_count);
            let legacy_ref = checkpoint::checkpoint_ref(session_id, turn_count - 1);
            let to_ref = checkpoint::checkpoint_ref(session_id, turn_count);
            Range {
                from: resolve(cwd, &diff_base_ref)
                    .or_else(|| resolve(cwd, &start_ref))
                    .or_else(|| resolve(cwd, &legacy_ref))
                    .ok_or_else(|| anyhow!("the turn's starting checkpoint is unavailable"))?,
                to: resolve(cwd, &to_ref)
                    .ok_or_else(|| anyhow!("the turn's ending checkpoint is unavailable"))?,
            }
        }
        Source::Uncommitted => Range {
            from: head,
            to: checkpoint::capture_worktree_commit(cwd)?,
        },
        Source::Unstaged => Range {
            from: index_tree(cwd)?,
            to: checkpoint::capture_worktree_commit(cwd)?,
        },
        Source::Staged => Range {
            from: head,
            to: index_tree(cwd)?,
        },
        Source::Committed => Range {
            from: branch_base(cwd)?,
            to: head,
        },
        Source::Branch => Range {
            from: branch_base(cwd)?,
            to: checkpoint::capture_worktree_commit(cwd)?,
        },
    })
}

fn branch_base(cwd: &Path) -> anyhow::Result<String> {
    let Some(snapshot) = git_branch::inspect(cwd)? else {
        bail!("the workspace is not a Git repository");
    };
    let Some(head) = resolve(cwd, "HEAD") else {
        return Ok(EMPTY_TREE.to_owned());
    };
    let current = snapshot.current.as_deref();
    let default_branch = snapshot
        .default_branch
        .filter(|branch| current != Some(branch.as_str()))
        .or_else(|| {
            ["main", "master"]
                .into_iter()
                .find(|candidate| {
                    current != Some(*candidate)
                        && snapshot
                            .branches
                            .iter()
                            .any(|branch| branch.name == *candidate)
                })
                .map(str::to_owned)
        });
    let Some(default_branch) = default_branch else {
        return Ok(head);
    };
    let output = git(cwd, ["merge-base", "HEAD", default_branch.as_str()])?;
    let base = output.trim();
    Ok(if base.is_empty() {
        head
    } else {
        base.to_owned()
    })
}

fn index_tree(cwd: &Path) -> anyhow::Result<String> {
    let output = Command::new("git")
        .args(["write-tree"])
        .current_dir(cwd)
        .output()
        .context("failed to snapshot the Git index")?;
    if output.status.success() {
        let tree = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if !tree.is_empty() {
            return Ok(tree);
        }
    }
    // A repository with no index represents an empty staged tree.
    if resolve(cwd, "HEAD").is_none() {
        Ok(EMPTY_TREE.to_owned())
    } else {
        bail!("{}", command_error(&output))
    }
}

fn diff_output(cwd: &Path, range: &Range, modes: &[&str]) -> anyhow::Result<String> {
    let mut command = Command::new("git");
    command
        .args([
            "-c",
            "core.quotePath=false",
            "diff",
            "--no-ext-diff",
            "--no-color",
        ])
        .args(modes)
        // Treat renames as a deletion plus an addition. It keeps the patch and
        // numstat path sets one-to-one, including paths containing spaces.
        .arg("--no-renames")
        .arg(&range.from)
        .arg(&range.to)
        .args(["--", "."])
        .current_dir(cwd);
    let output = command.output().context("failed to generate Git diff")?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        bail!("{}", command_error(&output))
    }
}

fn parse(source: Source, numstat: &str, patch: &str, complete_context: bool) -> Snapshot {
    let mut files = parse_numstat(numstat);
    let mut path_indexes = files
        .iter()
        .enumerate()
        .map(|(index, file)| (file.path.clone(), index))
        .collect::<HashMap<_, _>>();
    let mut lines = Vec::new();
    let mut current_file = None;
    let mut old_line = 0u32;
    let mut new_line = 0u32;
    let mut previous_old_next = 1u32;
    let mut previous_new_next = 1u32;
    let mut old_carry = Carry::None;
    let mut new_carry = Carry::None;
    let mut next_gap_id = 0u64;

    for raw in patch.lines() {
        if let Some(path) = parse_diff_header_path(raw) {
            let file_index = path_indexes.get(&path).copied().unwrap_or_else(|| {
                let index = files.len();
                files.push(File {
                    path: path.clone(),
                    additions: 0,
                    deletions: 0,
                    status: FileStatus::Modified,
                    diff_line: None,
                });
                path_indexes.insert(path, index);
                index
            });
            current_file = Some(file_index);
            old_line = 0;
            new_line = 0;
            previous_old_next = 1;
            previous_new_next = 1;
            old_carry = Carry::None;
            new_carry = Carry::None;
            lines.push(Line {
                file_index,
                old_line: None,
                new_line: None,
                kind: LineKind::FileHeader,
                content: String::new(),
                tokens: Vec::new(),
            });
            continue;
        }
        let Some(file_index) = current_file else {
            continue;
        };

        if raw.starts_with("new file mode ") {
            files[file_index].status = FileStatus::Added;
            continue;
        }
        if raw.starts_with("deleted file mode ") {
            files[file_index].status = FileStatus::Deleted;
            continue;
        }
        if raw.starts_with("Binary files ") || raw == "GIT binary patch" {
            files[file_index].status = FileStatus::Binary;
            lines.push(Line {
                file_index,
                old_line: None,
                new_line: None,
                kind: LineKind::Meta,
                content: "Binary file changed".into(),
                tokens: Vec::new(),
            });
            continue;
        }
        if raw.starts_with("index ")
            || raw.starts_with("--- ")
            || raw.starts_with("+++ ")
            || raw.starts_with("old mode ")
            || raw.starts_with("new mode ")
        {
            continue;
        }

        if let Some((next_old, next_new)) = parse_hunk_starts(raw) {
            let old_gap = next_old.saturating_sub(previous_old_next);
            let new_gap = next_new.saturating_sub(previous_new_next);
            let gap = old_gap.max(new_gap);
            if !complete_context && gap > 0 {
                let first_hunk = previous_old_next == 1 && previous_new_next == 1;
                lines.push(Line {
                    file_index,
                    old_line: None,
                    new_line: None,
                    kind: LineKind::Gap(Gap {
                        id: next_gap_id,
                        count: gap,
                        hidden: Vec::new(),
                        position: if first_hunk {
                            GapPosition::Leading
                        } else {
                            GapPosition::Between
                        },
                    }),
                    content: String::new(),
                    tokens: Vec::new(),
                });
                next_gap_id = next_gap_id.wrapping_add(1);
            } else if !complete_context && (previous_old_next != 1 || previous_new_next != 1) {
                lines.push(Line {
                    file_index,
                    old_line: None,
                    new_line: None,
                    kind: LineKind::HunkHeader,
                    content: raw.to_owned(),
                    tokens: Vec::new(),
                });
            }
            old_line = next_old;
            new_line = next_new;
            old_carry = Carry::None;
            new_carry = Carry::None;
            continue;
        }

        let Some(marker) = raw.as_bytes().first().copied() else {
            continue;
        };
        let content = raw.get(1..).unwrap_or_default().to_owned();
        let language = language_for_path(&files[file_index].path);
        let (kind, shown_old, shown_new, tokens) = match marker {
            b' ' => {
                let (tokens, next_new_carry) = tokenize(language, &content, new_carry);
                let (_, next_old_carry) = tokenize(language, &content, old_carry);
                let shown = (Some(old_line), Some(new_line));
                old_line = old_line.saturating_add(1);
                new_line = new_line.saturating_add(1);
                old_carry = next_old_carry;
                new_carry = next_new_carry;
                (LineKind::Context, shown.0, shown.1, tokens)
            }
            b'-' => {
                let (tokens, carry) = tokenize(language, &content, old_carry);
                let shown = old_line;
                old_line = old_line.saturating_add(1);
                old_carry = carry;
                (LineKind::Deletion, Some(shown), None, tokens)
            }
            b'+' => {
                let (tokens, carry) = tokenize(language, &content, new_carry);
                let shown = new_line;
                new_line = new_line.saturating_add(1);
                new_carry = carry;
                (LineKind::Addition, None, Some(shown), tokens)
            }
            b'\\' => (LineKind::Meta, None, None, Vec::new()),
            _ => continue,
        };
        previous_old_next = old_line;
        previous_new_next = new_line;
        lines.push(Line {
            file_index,
            old_line: shown_old,
            new_line: shown_new,
            kind,
            content,
            tokens,
        });
    }

    let lines = if complete_context {
        collapse_context(lines)
    } else {
        lines
    };
    let (lines, truncated) = cap_visible_lines(lines);
    let max_content_columns = max_content_columns(&lines);
    recompute_diff_lines(&mut files, &lines);
    let additions = files.iter().map(|file| file.additions).sum();
    let deletions = files.iter().map(|file| file.deletions).sum();
    Snapshot {
        source,
        files,
        lines,
        max_content_columns,
        additions,
        deletions,
        truncated,
    }
}

/// Widest line among everything the list can ever show: the rows themselves
/// plus the context retained inside collapsed gaps, so expanding a gap never
/// changes the horizontal extent. Rows dropped by [`cap_visible_lines`] are
/// excluded — they would only buy dead scroll range.
fn max_content_columns(lines: &[Line]) -> usize {
    lines
        .iter()
        .map(|line| {
            let hidden = match &line.kind {
                LineKind::Gap(gap) => gap
                    .hidden
                    .iter()
                    .map(|line| monospace_columns(&line.content))
                    .max()
                    .unwrap_or_default(),
                _ => 0,
            };
            monospace_columns(&line.content).max(hidden)
        })
        .max()
        .unwrap_or_default()
}

/// Columns a rendered line occupies, where one column is one advance of the
/// mono font.
///
/// Nothing expands tabs before shaping, and the mono font advances a tab by a
/// single column, so that is what a tab costs here — charging it a tab stop
/// would reserve scroll range that no glyph ever reaches. Wide characters are
/// the only ones worth two: they are never wider than two advances, so the
/// result stays an upper bound on the painted width.
fn monospace_columns(text: &str) -> usize {
    text.chars()
        .map(|ch| UnicodeWidthChar::width(ch).unwrap_or(1))
        .sum()
}

fn collapse_context(lines: Vec<Line>) -> Vec<Line> {
    let mut collapsed = Vec::new();
    let mut next_gap_id = 0u64;
    let mut file_start = 0;
    while file_start < lines.len() {
        let file_index = lines[file_start].file_index;
        let mut file_end = file_start + 1;
        while file_end < lines.len() && lines[file_end].file_index == file_index {
            file_end += 1;
        }
        collapse_file_context(
            &lines[file_start..file_end],
            &mut collapsed,
            &mut next_gap_id,
        );
        file_start = file_end;
    }
    collapsed
}

fn collapse_file_context(lines: &[Line], output: &mut Vec<Line>, next_gap_id: &mut u64) {
    let mut change_after = vec![false; lines.len() + 1];
    for index in (0..lines.len()).rev() {
        change_after[index] = change_after[index + 1] || is_change_line(&lines[index]);
    }

    let mut saw_change = false;
    let mut index = 0;
    while index < lines.len() {
        if lines[index].kind != LineKind::Context {
            saw_change |= is_change_line(&lines[index]);
            output.push(lines[index].clone());
            index += 1;
            continue;
        }

        let run_start = index;
        while index < lines.len() && lines[index].kind == LineKind::Context {
            index += 1;
        }
        let run = &lines[run_start..index];
        let has_later_change = change_after[index];
        match (saw_change, has_later_change) {
            (false, true) => collapse_leading_context(run, output, next_gap_id),
            (true, false) => collapse_trailing_context(run, output, next_gap_id),
            (true, true) => collapse_between_context(run, output, next_gap_id),
            (false, false) => output.extend_from_slice(run),
        }
    }
}

fn collapse_leading_context(lines: &[Line], output: &mut Vec<Line>, next_gap_id: &mut u64) {
    let kept = COLLAPSED_CONTEXT_LINES.min(lines.len());
    let hidden = &lines[..lines.len() - kept];
    if hidden.len() <= COLLAPSED_CONTEXT_THRESHOLD {
        output.extend_from_slice(lines);
        return;
    }
    push_context_gap(output, hidden, GapPosition::Leading, next_gap_id);
    output.extend_from_slice(&lines[lines.len() - kept..]);
}

fn collapse_trailing_context(lines: &[Line], output: &mut Vec<Line>, next_gap_id: &mut u64) {
    let kept = COLLAPSED_CONTEXT_LINES.min(lines.len());
    let hidden = &lines[kept..];
    if hidden.len() <= COLLAPSED_CONTEXT_THRESHOLD {
        output.extend_from_slice(lines);
        return;
    }
    output.extend_from_slice(&lines[..kept]);
    push_context_gap(output, hidden, GapPosition::Trailing, next_gap_id);
}

fn collapse_between_context(lines: &[Line], output: &mut Vec<Line>, next_gap_id: &mut u64) {
    let kept_start = COLLAPSED_CONTEXT_LINES.min(lines.len());
    let kept_end = COLLAPSED_CONTEXT_LINES.min(lines.len().saturating_sub(kept_start));
    let hidden = &lines[kept_start..lines.len() - kept_end];
    if hidden.len() <= COLLAPSED_CONTEXT_THRESHOLD {
        output.extend_from_slice(lines);
        return;
    }
    output.extend_from_slice(&lines[..kept_start]);
    push_context_gap(output, hidden, GapPosition::Between, next_gap_id);
    output.extend_from_slice(&lines[lines.len() - kept_end..]);
}

fn push_context_gap(
    output: &mut Vec<Line>,
    hidden: &[Line],
    position: GapPosition,
    next_gap_id: &mut u64,
) {
    let Some(first) = hidden.first() else {
        return;
    };
    let count = hidden.len().min(u32::MAX as usize) as u32;
    output.push(Line {
        file_index: first.file_index,
        old_line: None,
        new_line: None,
        kind: LineKind::Gap(Gap {
            id: *next_gap_id,
            count,
            hidden: hidden[..count as usize].to_vec(),
            position,
        }),
        content: String::new(),
        tokens: Vec::new(),
    });
    *next_gap_id = next_gap_id.wrapping_add(1);
}

fn is_change_line(line: &Line) -> bool {
    matches!(line.kind, LineKind::Addition | LineKind::Deletion)
}

fn cap_visible_lines(lines: Vec<Line>) -> (Vec<Line>, bool) {
    let truncated = lines.len() > MAX_RENDERED_DIFF_LINES;
    let mut visible = Vec::with_capacity(lines.len().min(MAX_RENDERED_DIFF_LINES));
    for line in lines {
        if !push_line(&mut visible, line) {
            break;
        }
    }
    (visible, truncated)
}

fn recompute_diff_lines(files: &mut [File], lines: &[Line]) {
    for file in files.iter_mut() {
        file.diff_line = None;
    }
    for (line_index, line) in lines.iter().enumerate() {
        if line.kind == LineKind::FileHeader
            && let Some(file) = files.get_mut(line.file_index)
        {
            file.diff_line.get_or_insert(line_index);
        }
    }
}

fn push_line(lines: &mut Vec<Line>, line: Line) -> bool {
    if lines.len() >= MAX_RENDERED_DIFF_LINES {
        false
    } else {
        lines.push(line);
        true
    }
}

fn parse_numstat(output: &str) -> Vec<File> {
    output
        .lines()
        .filter_map(|line| {
            let mut columns = line.splitn(3, '\t');
            let additions = columns.next()?;
            let deletions = columns.next()?;
            let path = columns.next()?.to_owned();
            Some(File {
                path,
                additions: additions.parse().unwrap_or(0),
                deletions: deletions.parse().unwrap_or(0),
                status: if additions == "-" || deletions == "-" {
                    FileStatus::Binary
                } else {
                    FileStatus::Modified
                },
                diff_line: None,
            })
        })
        .collect()
}

fn parse_diff_header_path(line: &str) -> Option<String> {
    let rest = line.strip_prefix("diff --git ")?;
    let path = if let Some((_, right)) = rest.rsplit_once(" b/") {
        right
    } else if let Some((_, right)) = rest.rsplit_once(" \"b/") {
        right.strip_suffix('"').unwrap_or(right)
    } else {
        return None;
    };
    Some(unescape_git_path(path))
}

fn unescape_git_path(path: &str) -> String {
    let mut output = String::with_capacity(path.len());
    let mut chars = path.chars();
    while let Some(character) = chars.next() {
        if character == '\\' {
            if let Some(escaped) = chars.next() {
                output.push(match escaped {
                    't' => '\t',
                    'n' => '\n',
                    'r' => '\r',
                    '\\' => '\\',
                    '"' => '"',
                    other => other,
                });
            }
        } else {
            output.push(character);
        }
    }
    output
}

fn parse_hunk_starts(line: &str) -> Option<(u32, u32)> {
    let ranges = line.strip_prefix("@@ ")?.split_once(" @@")?.0;
    let mut parts = ranges.split_whitespace();
    let old = parts.next()?.strip_prefix('-')?;
    let new = parts.next()?.strip_prefix('+')?;
    Some((parse_range_start(old)?, parse_range_start(new)?))
}

fn parse_range_start(range: &str) -> Option<u32> {
    range.split(',').next()?.parse().ok()
}

fn language_for_path(path: &str) -> Option<Lang> {
    let path = Path::new(path);
    let name = path.file_name()?.to_str()?;
    let normalized = name.to_ascii_lowercase();
    let tag = match normalized.as_str() {
        "makefile" => "make",
        "dockerfile" => "dockerfile",
        "cargo.lock" => "toml",
        "package-lock.json" | "composer.lock" => "json",
        _ => path.extension()?.to_str()?,
    };
    lang_for_tag(tag)
}

fn tokenize(language: Option<Lang>, content: &str, carry: Carry) -> (Vec<Token>, Carry) {
    language.map_or_else(
        || (Vec::new(), Carry::None),
        |language| tokenize_line(language, content, carry),
    )
}

fn ensure_repository(cwd: &Path) -> anyhow::Result<()> {
    let output = Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(cwd)
        .output()
        .context("failed to inspect Git workspace")?;
    if output.status.success() {
        Ok(())
    } else {
        bail!("the workspace is not a Git repository")
    }
}

fn resolve(cwd: &Path, revision: &str) -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", "--verify", &format!("{revision}^{{commit}}")])
        .current_dir(cwd)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn git<I, S>(cwd: &Path, args: I) -> anyhow::Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .context("failed to execute git")?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        bail!("{}", command_error(&output))
    }
}

fn command_error(output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if stderr.is_empty() {
        format!("git exited with {}", output.status)
    } else {
        stderr
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn source_columns_match_the_advances_the_mono_font_paints() {
        // A tab advances one column in the mono font, and nothing expands tabs
        // before shaping, so a tab-indented line must not be charged a tab stop.
        assert_eq!(monospace_columns("ab\tc"), 4);
        // Accented Latin and arrows are single-advance despite not being ASCII.
        assert_eq!(monospace_columns("é→"), 2);
        // Wide characters are the only ones worth two.
        assert_eq!(monospace_columns("a界b"), 4);
    }

    fn git_ok(cwd: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", command_error(&output));
    }

    fn repository() -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("waku-review-diff-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        git_ok(&root, &["init", "-b", "main"]);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/lib.rs"), "fn baseline() {}\n").unwrap();
        git_ok(&root, &["add", "."]);
        git_ok(
            &root,
            &[
                "-c",
                "user.name=Waku Tests",
                "-c",
                "user.email=waku@example.com",
                "commit",
                "-m",
                "baseline",
            ],
        );
        root
    }

    #[test]
    fn parses_file_headers_gaps_line_numbers_and_syntax() {
        let numstat = "2\t1\tsrc/lib.rs\n";
        let patch = r#"diff --git a/src/lib.rs b/src/lib.rs
index 1111111..2222222 100644
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -5,2 +5,3 @@
-let old = 1;
+let fresh = 2;
+return fresh;
 context();
"#;
        let snapshot = parse(Source::Uncommitted, numstat, patch, false);
        assert_eq!(snapshot.files.len(), 1);
        assert_eq!(snapshot.files[0].diff_line, Some(0));
        assert!(matches!(
            &snapshot.lines[1].kind,
            LineKind::Gap(gap) if gap.count() == 4
        ));
        assert_eq!(snapshot.lines[2].old_line, Some(5));
        assert_eq!(snapshot.lines[3].new_line, Some(5));
        assert!(
            snapshot.lines[3]
                .tokens
                .iter()
                .any(|token| token.class == crate::md::highlight::TokenClass::Keyword)
        );
        assert_eq!((snapshot.additions, snapshot.deletions), (2, 1));
    }

    fn full_patch(total_lines: u32, changes: &[u32]) -> String {
        let mut patch = format!(
            "diff --git a/src/lib.rs b/src/lib.rs\n\
             index 1111111..2222222 100644\n\
             --- a/src/lib.rs\n\
             +++ b/src/lib.rs\n\
             @@ -1,{total_lines} +1,{total_lines} @@\n"
        );
        for line in 1..=total_lines {
            if changes.contains(&line) {
                patch.push_str(&format!("-let value_{line} = \"old\";\n"));
                patch.push_str(&format!("+let value_{line} = \"new\";\n"));
            } else {
                patch.push_str(&format!(" line {line}\n"));
            }
        }
        patch
    }

    #[test]
    fn full_context_collapses_around_changes_and_label_expands_everything() {
        let patch = full_patch(30, &[8, 17]);
        let mut snapshot = parse(Source::Uncommitted, "2\t2\tsrc/lib.rs\n", &patch, true);
        let gaps = snapshot
            .lines
            .iter()
            .filter_map(|line| match &line.kind {
                LineKind::Gap(gap) => Some((gap.count(), gap.position)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            gaps,
            vec![
                (4, GapPosition::Leading),
                (2, GapPosition::Between),
                (10, GapPosition::Trailing),
            ]
        );

        let gap_index = snapshot
            .lines
            .iter()
            .position(|line| {
                matches!(&line.kind, LineKind::Gap(gap) if gap.position == GapPosition::Between)
            })
            .unwrap();
        let previous_len = snapshot.lines.len();
        let expansion = snapshot
            .expand_gap(gap_index, ExpansionDirection::All)
            .unwrap();
        assert_eq!(expansion.replacement_count, 2);
        assert_eq!(snapshot.lines.len(), previous_len + 1);
        assert!(snapshot.lines.iter().any(|line| line.content == "line 12"));
        assert!(snapshot.lines.iter().any(|line| line.content == "line 13"));
    }

    #[test]
    fn directional_expansion_reveals_one_hundred_lines_from_the_gap_edge() {
        let patch = full_patch(230, &[221]);
        let mut snapshot = parse(Source::Uncommitted, "1\t1\tsrc/lib.rs\n", &patch, true);
        let gap_index = snapshot
            .lines
            .iter()
            .position(|line| matches!(line.kind, LineKind::Gap(_)))
            .unwrap();
        let first = snapshot
            .expand_gap(gap_index, ExpansionDirection::End)
            .unwrap();
        assert_eq!(first.replacement_count, 101);
        let LineKind::Gap(gap) = &snapshot.lines[gap_index].kind else {
            panic!("leading gap remains after the first chunk")
        };
        assert_eq!(gap.count(), 117);
        assert_eq!(snapshot.lines[gap_index + 1].new_line, Some(118));

        let second = snapshot
            .expand_gap(gap_index, ExpansionDirection::End)
            .unwrap();
        assert_eq!(second.replacement_count, 101);
        let LineKind::Gap(gap) = &snapshot.lines[gap_index].kind else {
            panic!("leading gap remains after the second chunk")
        };
        assert_eq!(gap.count(), 17);
        assert_eq!(snapshot.lines[gap_index + 1].new_line, Some(18));

        let third = snapshot
            .expand_gap(gap_index, ExpansionDirection::End)
            .unwrap();
        assert_eq!(third.replacement_count, 17);
        assert!(matches!(snapshot.lines[gap_index].kind, LineKind::Context));
        assert_eq!(snapshot.lines[gap_index].new_line, Some(1));
    }

    #[test]
    fn count_expansion_reveals_one_hundred_lines_from_both_edges() {
        let patch = full_patch(230, &[221]);
        let mut snapshot = parse(Source::Uncommitted, "1\t1\tsrc/lib.rs\n", &patch, true);
        let gap_index = snapshot
            .lines
            .iter()
            .position(|line| matches!(line.kind, LineKind::Gap(_)))
            .unwrap();

        let expansion = snapshot
            .expand_gap(gap_index, ExpansionDirection::Both)
            .unwrap();
        assert_eq!(expansion.replacement_count, 201);
        assert_eq!(snapshot.lines[gap_index].new_line, Some(1));
        let LineKind::Gap(gap) = &snapshot.lines[gap_index + 100].kind else {
            panic!("the unrevealed center remains collapsed")
        };
        assert_eq!(gap.count(), 17);
        assert_eq!(snapshot.lines[gap_index + 101].new_line, Some(118));
    }

    #[test]
    fn source_modes_compare_consistent_git_snapshots() {
        let root = repository();
        git_ok(&root, &["switch", "-c", "feature"]);
        fs::write(root.join("src/lib.rs"), "fn committed() {}\n").unwrap();
        git_ok(&root, &["add", "src/lib.rs"]);
        git_ok(
            &root,
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
        fs::write(
            root.join("src/lib.rs"),
            "fn committed() {}\nfn staged() {}\n",
        )
        .unwrap();
        git_ok(&root, &["add", "src/lib.rs"]);
        fs::write(
            root.join("src/lib.rs"),
            "fn committed() {}\nfn staged() {}\nfn unstaged() {}\n",
        )
        .unwrap();
        fs::write(root.join("new file.txt"), "untracked\n").unwrap();

        let committed = collect(&root, Source::Committed).unwrap();
        let staged = collect(&root, Source::Staged).unwrap();
        let unstaged = collect(&root, Source::Unstaged).unwrap();
        let uncommitted = collect(&root, Source::Uncommitted).unwrap();
        let branch = collect(&root, Source::Branch).unwrap();

        assert_eq!(committed.files.len(), 1);
        assert_eq!(staged.files.len(), 1);
        assert_eq!(unstaged.files.len(), 2, "unstaged includes untracked files");
        assert_eq!(uncommitted.files.len(), 2);
        assert_eq!(branch.files.len(), 2);
        assert_eq!((committed.additions, committed.deletions), (1, 1));
        assert_eq!((staged.additions, staged.deletions), (1, 0));
        assert_eq!((unstaged.additions, unstaged.deletions), (2, 0));
        assert_eq!((uncommitted.additions, uncommitted.deletions), (3, 0));
        assert_eq!((branch.additions, branch.deletions), (4, 1));
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn last_turn_uses_captured_checkpoints_not_the_live_worktree() {
        let root = repository();
        let session_id = Uuid::new_v4();
        let turn_id = Uuid::new_v4();
        checkpoint::capture_turn(&root, session_id, 0).unwrap();
        fs::write(
            root.join("src/lib.rs"),
            "fn baseline() {}\nfn from_turn() {}\n",
        )
        .unwrap();
        checkpoint::capture_turn(&root, session_id, 1).unwrap();
        fs::write(
            root.join("src/lib.rs"),
            "fn baseline() {}\nfn from_turn() {}\nfn after_turn() {}\n",
        )
        .unwrap();

        let snapshot = collect(
            &root,
            Source::LastTurn {
                session_id,
                turn_id,
                turn_count: 1,
            },
        )
        .unwrap();
        assert_eq!((snapshot.additions, snapshot.deletions), (1, 0));
        assert!(
            snapshot
                .lines
                .iter()
                .any(|line| line.content.contains("from_turn"))
        );
        assert!(
            snapshot
                .lines
                .iter()
                .all(|line| !line.content.contains("after_turn"))
        );
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn last_turn_review_uses_the_branch_aware_diff_base() {
        let root = repository();
        git_ok(&root, &["switch", "-c", "feature"]);
        fs::write(root.join("feature-only.rs"), "fn feature() {}\n").unwrap();
        git_ok(&root, &["add", "feature-only.rs"]);
        git_ok(
            &root,
            &[
                "-c",
                "user.name=Waku Tests",
                "-c",
                "user.email=waku@example.com",
                "commit",
                "-m",
                "feature baseline",
            ],
        );
        git_ok(&root, &["switch", "main"]);
        fs::write(root.join("main-only.rs"), "fn main_only() {}\n").unwrap();
        git_ok(&root, &["add", "main-only.rs"]);
        git_ok(
            &root,
            &[
                "-c",
                "user.name=Waku Tests",
                "-c",
                "user.email=waku@example.com",
                "commit",
                "-m",
                "main baseline",
            ],
        );

        let session_id = Uuid::new_v4();
        let turn_id = Uuid::new_v4();
        checkpoint::capture_turn_start(&root, session_id, 1).unwrap();
        git_ok(&root, &["switch", "feature"]);
        fs::write(
            root.join("src/lib.rs"),
            "fn baseline() {}\nfn from_turn() {}\n",
        )
        .unwrap();
        checkpoint::capture_turn(&root, session_id, 1).unwrap();

        let snapshot = collect(
            &root,
            Source::LastTurn {
                session_id,
                turn_id,
                turn_count: 1,
            },
        )
        .unwrap();
        assert_eq!(snapshot.files.len(), 1);
        assert_eq!(snapshot.files[0].path, "src/lib.rs");
        assert_eq!((snapshot.additions, snapshot.deletions), (1, 0));
        fs::remove_dir_all(root).ok();
    }
}

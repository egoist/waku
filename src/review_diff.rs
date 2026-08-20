//! Immutable, render-ready Git diffs for Waku's Review surface.
//!
//! The daemon captures Git output. This desktop module only parses and expands
//! that returned data off the UI thread; a frame only indexes stored rows.

use std::collections::HashMap;
use std::path::Path;

use uuid::Uuid;

use crate::md::highlight::{Carry, Lang, Token, lang_for_tag, tokenize_line};

const COLLAPSED_CONTEXT_LINES: usize = 3;
const COLLAPSED_CONTEXT_THRESHOLD: usize = 1;
/// Pierre expands a directional hunk control in 100-line increments. The
/// count label itself expands the complete region.
pub const DEFAULT_EXPANSION_LINE_COUNT: usize = 100;
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

/// Turn daemon-captured Git output into render-ready rows. Parsing and syntax
/// tokenization remain client-side presentation work; no path is resolved and
/// no subprocess is started here.
pub fn parse_collected(
    source: Source,
    numstat: &str,
    patch: &str,
    complete_context: bool,
) -> Snapshot {
    let mut snapshot = parse(source, numstat, patch, complete_context);
    if !complete_context {
        snapshot.truncated = true;
    }
    snapshot
}

/// A snapshot over the file changes a provider reported for one tool call.
///
/// The normalization that produced those bodies already ran when the tool
/// event arrived, so this only reassembles them into the patch text [`parse`]
/// already understands. `Source` is meaningless here — nothing reads it on a
/// tool-call diff — and context is never complete, because the provider sent
/// hunks rather than whole files.
pub fn from_file_changes(changes: &[crate::model::ActivityFileChange]) -> Snapshot {
    let mut numstat = String::new();
    let mut patch = String::new();
    for change in changes {
        let Some(body) = change.diff.as_deref() else {
            continue;
        };
        numstat.push_str(&format!(
            "{}\t{}\t{}\n",
            change.additions.unwrap_or(0),
            change.deletions.unwrap_or(0),
            change.path
        ));
        patch.push_str(&format!(
            "diff --git a/{path} b/{path}\n",
            path = change.path
        ));
        match change.status {
            Some(crate::model::ActivityFileChangeStatus::Added) => {
                patch.push_str("new file mode 100644\n");
            }
            Some(crate::model::ActivityFileChangeStatus::Deleted) => {
                patch.push_str("deleted file mode 100644\n");
            }
            _ => {}
        }
        patch.push_str(body);
        if !body.ends_with('\n') {
            patch.push('\n');
        }
    }
    parse(Source::default(), &numstat, &patch, false)
}

pub fn wire_source(source: Source) -> waku_client::workspace::ReviewDiffSource {
    match source {
        Source::LastTurn {
            session_id,
            turn_id,
            turn_count,
        } => waku_client::workspace::ReviewDiffSource::LastTurn {
            session_id,
            turn_id,
            turn_count,
        },
        Source::Uncommitted => waku_client::workspace::ReviewDiffSource::Uncommitted,
        Source::Unstaged => waku_client::workspace::ReviewDiffSource::Unstaged,
        Source::Staged => waku_client::workspace::ReviewDiffSource::Staged,
        Source::Committed => waku_client::workspace::ReviewDiffSource::Committed,
        Source::Branch => waku_client::workspace::ReviewDiffSource::Branch,
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
    // Whether the current hunk told us where in the file it sits.
    let mut positioned = true;

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
            positioned = true;
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

        // A hunk header with no ranges. Git never writes one, but a diff
        // synthesized from a provider's before/after text has no position to
        // report, so its rows are numbered `None` rather than from 1.
        if raw.starts_with("@@") && parse_hunk_starts(raw).is_none() {
            positioned = false;
            if !lines
                .last()
                .is_none_or(|line| line.kind == LineKind::FileHeader)
            {
                lines.push(Line {
                    file_index,
                    old_line: None,
                    new_line: None,
                    kind: LineKind::HunkHeader,
                    content: String::new(),
                    tokens: Vec::new(),
                });
            }
            old_carry = Carry::None;
            new_carry = Carry::None;
            continue;
        }

        if let Some((next_old, next_new)) = parse_hunk_starts(raw) {
            positioned = true;
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
                let shown = (
                    positioned.then_some(old_line),
                    positioned.then_some(new_line),
                );
                old_line = old_line.saturating_add(1);
                new_line = new_line.saturating_add(1);
                old_carry = next_old_carry;
                new_carry = next_new_carry;
                (LineKind::Context, shown.0, shown.1, tokens)
            }
            b'-' => {
                let (tokens, carry) = tokenize(language, &content, old_carry);
                let shown = positioned.then_some(old_line);
                old_line = old_line.saturating_add(1);
                old_carry = carry;
                (LineKind::Deletion, shown, None, tokens)
            }
            b'+' => {
                let (tokens, carry) = tokenize(language, &content, new_carry);
                let shown = positioned.then_some(new_line);
                new_line = new_line.saturating_add(1);
                new_carry = carry;
                (LineKind::Addition, None, shown, tokens)
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
    recompute_diff_lines(&mut files, &lines);
    let additions = files.iter().map(|file| file.additions).sum();
    let deletions = files.iter().map(|file| file.deletions).sum();
    Snapshot {
        source,
        files,
        lines,
        additions,
        deletions,
        truncated,
    }
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

pub fn parse_hunk_starts(line: &str) -> Option<(u32, u32)> {
    let ranges = line.strip_prefix("@@ ")?.split_once(" @@")?.0;
    let mut parts = ranges.split_whitespace();
    let old = parts.next()?.strip_prefix('-')?;
    let new = parts.next()?.strip_prefix('+')?;
    Some((parse_range_start(old)?, parse_range_start(new)?))
}

fn parse_range_start(range: &str) -> Option<u32> {
    range.split(',').next()?.parse().ok()
}

pub fn language_for_path(path: &str) -> Option<Lang> {
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

pub fn tokenize(language: Option<Lang>, content: &str, carry: Carry) -> (Vec<Token>, Carry) {
    language.map_or_else(
        || (Vec::new(), Carry::None),
        |language| tokenize_line(language, content, carry),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

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
}

//! The diff shown inside an expanded file-change activity.
//!
//! There is nothing to parse here: provider payloads are normalized into
//! unified-diff bodies when the tool event arrives, and
//! [`review_diff::from_file_changes`] turns those into the same positioned,
//! syntax-tokenized rows the Review panel reads. This module only decides how
//! much of that snapshot a transcript row should carry.

use crate::model::ActivityItem;
use crate::review_diff::{self, LineKind, Snapshot};

/// A diff inside a transcript row is a summary, not a review surface: past
/// this many rows, Review is where the change should be read.
const MAX_ROWS: usize = 400;

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Diff {
    pub(super) snapshot: Snapshot,
    /// Rows dropped at [`MAX_ROWS`], so the card can say so instead of
    /// quietly presenting part of a change as the whole one.
    pub(super) hidden_rows: usize,
}

impl Diff {
    pub(super) fn is_empty(&self) -> bool {
        self.snapshot.lines.is_empty()
    }

    /// Whether any row knows where it sits in its file. Providers that only
    /// report before/after text leave every row unpositioned, and the gutter
    /// falls back to the `+`/`-` marker.
    #[cfg(test)]
    fn has_line_numbers(&self) -> bool {
        self.snapshot
            .lines
            .iter()
            .any(|line| line.old_line.is_some() || line.new_line.is_some())
    }
}

/// Build the rows for one activity's file changes.
///
/// Runs once when the activity is expanded — never from a row builder — and
/// the caller keeps the result until the activity's changes are replaced.
pub(super) fn build(activity: &ActivityItem) -> Diff {
    let mut snapshot = review_diff::from_file_changes(&activity.file_changes);
    // One file needs no header: the activity's own row already names it.
    if snapshot.files.len() < 2 {
        snapshot
            .lines
            .retain(|line| line.kind != LineKind::FileHeader);
    }
    let hidden_rows = snapshot.lines.len().saturating_sub(MAX_ROWS);
    snapshot.lines.truncate(MAX_ROWS);
    Diff {
        snapshot,
        hidden_rows,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ActivityFileChange, ActivityKind};

    fn activity(changes: Vec<ActivityFileChange>) -> ActivityItem {
        let mut activity = ActivityItem::new(None, ActivityKind::FileChange, "Edit", None, true);
        activity.file_changes = changes;
        activity
    }

    fn change(path: &str, diff: &str) -> ActivityFileChange {
        ActivityFileChange {
            path: path.into(),
            additions: Some(1),
            deletions: Some(1),
            status: None,
            diff: Some(diff.into()),
        }
    }

    #[test]
    fn positioned_hunks_number_both_sides() {
        let diff = build(&activity(vec![change(
            "src/lib.rs",
            "@@ -10,3 +10,3 @@\n let kept = 1;\n-let old = 2;\n+let new = 2;\n",
        )]));

        let code = diff
            .snapshot
            .lines
            .iter()
            .filter(|line| {
                matches!(
                    line.kind,
                    LineKind::Context | LineKind::Addition | LineKind::Deletion
                )
            })
            .map(|line| (line.kind.clone(), line.old_line, line.new_line))
            .collect::<Vec<_>>();
        assert_eq!(
            code,
            vec![
                (LineKind::Context, Some(10), Some(10)),
                (LineKind::Deletion, Some(11), None),
                (LineKind::Addition, None, Some(11)),
            ]
        );
        assert!(diff.has_line_numbers());
        let context = diff
            .snapshot
            .lines
            .iter()
            .find(|line| line.kind == LineKind::Context)
            .expect("context row");
        assert_eq!(context.content, "let kept = 1;");
        assert!(!context.tokens.is_empty(), "Rust rows are highlighted");
    }

    #[test]
    fn positionless_hunks_render_without_inventing_line_numbers() {
        let diff = build(&activity(vec![change(
            "src/lib.rs",
            "@@\n-let old = 2;\n+let new = 2;\n",
        )]));

        assert!(!diff.has_line_numbers());
        assert!(
            diff.snapshot
                .lines
                .iter()
                .all(|line| line.old_line.is_none() && line.new_line.is_none())
        );
    }

    #[test]
    fn several_files_are_labeled_and_a_single_file_is_not() {
        let one = build(&activity(vec![change("src/one.rs", "@@\n+one\n")]));
        assert!(
            one.snapshot
                .lines
                .iter()
                .all(|line| line.kind != LineKind::FileHeader)
        );

        let two = build(&activity(vec![
            change("src/one.rs", "@@\n+one\n"),
            change("src/two.rs", "@@\n+two\n"),
        ]));
        let files = two
            .snapshot
            .lines
            .iter()
            .filter(|line| line.kind == LineKind::FileHeader)
            .filter_map(|line| two.snapshot.files.get(line.file_index))
            .map(|file| file.path.as_str())
            .collect::<Vec<_>>();
        assert_eq!(files, vec!["src/one.rs", "src/two.rs"]);
    }

    #[test]
    fn an_oversized_diff_is_capped_and_reports_what_it_dropped() {
        let body = std::iter::once("@@\n".to_owned())
            .chain((0..MAX_ROWS + 20).map(|line| format!("+line {line}\n")))
            .collect::<String>();

        let diff = build(&activity(vec![change("src/lib.rs", &body)]));

        assert_eq!(diff.snapshot.lines.len(), MAX_ROWS);
        assert_eq!(diff.hidden_rows, 20);
    }
}

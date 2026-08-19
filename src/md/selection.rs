//! Text selection spanning many painted text elements.
//!
//! GPUI has no built-in selection for text. Zed's markdown selects
//! continuously because its whole document is one element over one text model;
//! the transcript instead renders a *tree* of text elements inside a
//! virtualized list, so this module rebuilds that continuity.
//!
//! Every frame the renderer registers each painted text element in paint order
//! — which is document order — into a [`SelectionRegistry`]. A drag anchored in
//! one element resolves against that registry into per-element [`Span`]s:
//! partial in the anchor and head elements, whole for everything between. The
//! wash paints per element from its span, and copy joins the spans in order.
//!
//! This half is pure and gpui-free so it can be unit-tested; the geometry and
//! mouse listeners live in [`super::render`].

use std::cell::RefCell;
use std::ops::Range;
use std::rc::Rc;

/// Stable identity for one painted text element. `row` scopes it to a
/// transcript row so ids survive virtualized remounts; `index` orders elements
/// within that row.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct TextKey {
    pub row: Rc<str>,
    pub index: usize,
}

impl TextKey {
    pub fn new(row: impl Into<Rc<str>>, index: usize) -> Self {
        Self {
            row: row.into(),
            index,
        }
    }
}

/// One element's slice of the selection, in document order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Span {
    pub key: TextKey,
    /// Selected byte range of the element's flat text.
    pub range: Range<usize>,
    /// The element's full flat text. Snapshotted when the drag resolves, so
    /// copy still works after the element scrolls out of the registry.
    pub text: Rc<str>,
    /// True when this element starts a new block, so joined copy inserts a
    /// paragraph break rather than a single newline.
    pub block_break: bool,
}

/// The selection state for one transcript.
#[derive(Debug, Default)]
pub struct Selection {
    /// Element that owns the drag: where the mouse went down.
    anchor: Option<TextKey>,
    /// Byte offset of the anchor within its element.
    anchor_offset: usize,
    dragging: bool,
    /// Resolved spans in document order. Empty until a drag moves.
    spans: Vec<Span>,
}

impl Selection {
    pub fn is_empty(&self) -> bool {
        self.spans.iter().all(|span| span.range.is_empty())
    }

    /// Begin a drag anchored at `offset` in `key`.
    pub fn begin(&mut self, key: TextKey, offset: usize) {
        self.anchor = Some(key);
        self.anchor_offset = offset;
        self.dragging = true;
        self.spans.clear();
    }

    /// Begin with an immediate span: double- or triple-click in one element.
    pub fn begin_with_span(&mut self, key: TextKey, text: Rc<str>, range: Range<usize>) {
        self.anchor = Some(key.clone());
        self.anchor_offset = range.start;
        self.dragging = true;
        self.spans = vec![Span {
            key,
            range,
            text,
            block_break: false,
        }];
    }

    /// The live drag's anchor offset, if `key` owns the drag.
    pub fn drag_anchor(&self, key: &TextKey) -> Option<usize> {
        (self.dragging && self.anchor.as_ref() == Some(key)).then_some(self.anchor_offset)
    }

    pub fn anchor(&self) -> Option<&TextKey> {
        self.anchor.as_ref()
    }

    /// Replace the resolved spans. True when they changed and a repaint is due.
    pub fn set_spans(&mut self, spans: Vec<Span>) -> bool {
        if self.spans == spans {
            return false;
        }
        self.spans = spans;
        true
    }

    /// Finish `key`'s drag. Returns the selected text when non-empty.
    pub fn end_drag(&mut self, key: &TextKey) -> Option<String> {
        if self.anchor.as_ref() != Some(key) || !self.dragging {
            return None;
        }
        self.dragging = false;
        if self.is_empty() {
            self.clear();
            return None;
        }
        Some(self.text())
    }

    pub fn clear(&mut self) {
        self.anchor = None;
        self.anchor_offset = 0;
        self.dragging = false;
        self.spans.clear();
    }

    /// The wash range for `key` this frame. `None` means nothing to paint.
    pub fn wash_range(&self, key: &TextKey) -> Option<Range<usize>> {
        self.spans
            .iter()
            .find(|span| span.key == *key && !span.range.is_empty())
            .map(|span| span.range.clone())
    }

    /// The full selected text, or `None` when nothing is selected.
    pub fn selected_text(&self) -> Option<String> {
        (!self.is_empty()).then(|| self.text())
    }

    /// Selected spans belonging to one transcript row, preserving the stable
    /// flattened-text ordinal and byte offsets used by the renderer.
    pub fn spans_for_row(&self, row: &str) -> Vec<(usize, Range<usize>, Rc<str>, bool)> {
        self.spans
            .iter()
            .filter(|span| span.key.row.as_ref() == row)
            .map(|span| {
                (
                    span.key.index,
                    span.range.clone(),
                    span.text.clone(),
                    span.block_break,
                )
            })
            .collect()
    }

    /// The full selected text, spans joined in document order.
    pub fn text(&self) -> String {
        let mut out = String::new();
        let mut has_span = false;
        for span in &self.spans {
            if has_span {
                out.push('\n');
                if span.block_break {
                    out.push('\n');
                }
            }
            out.push_str(&span.text[span.range.clone()]);
            has_span = true;
        }
        out
    }
}

/// One painted text element as seen by the current frame. `G` carries whatever
/// geometry the renderer needs to hit-test it — a `TextLayout` in practice, and
/// `()` in tests, which keeps this module free of any UI dependency.
#[derive(Clone, Debug)]
pub struct RegisteredText<G = ()> {
    pub key: TextKey,
    pub text: Rc<str>,
    /// True when this element begins a markdown block, for copy spacing.
    pub block_break: bool,
    pub geometry: G,
}

/// The frame's document-ordered text elements. Paint order is document order,
/// so the renderer simply pushes as it paints and clears at the frame's start.
///
/// Holding the geometry here — rather than in each element's own closures — is
/// what lets the transcript install exactly one set of mouse listeners per
/// frame instead of three per painted text element.
#[derive(Debug)]
pub struct SelectionRegistry<G = ()> {
    entries: Vec<RegisteredText<G>>,
}

impl<G> Default for SelectionRegistry<G> {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
        }
    }
}

impl<G> SelectionRegistry<G> {
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    pub fn push(&mut self, entry: RegisteredText<G>) {
        self.entries.push(entry);
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn entries(&self) -> &[RegisteredText<G>] {
        &self.entries
    }

    pub fn position(&self, key: &TextKey) -> Option<usize> {
        self.entries.iter().position(|entry| entry.key == *key)
    }

    /// Resolve a selection between two `(element index, byte offset)` points
    /// into per-element spans. Either direction works; empty slices are
    /// skipped, and the first span never carries a block break.
    pub fn resolve(&self, a: (usize, usize), b: (usize, usize)) -> Vec<Span> {
        let (start, end) = if a <= b { (a, b) } else { (b, a) };
        let mut spans = Vec::new();
        let last = self.entries.len().saturating_sub(1);
        for index in start.0..=end.0.min(last) {
            let entry = &self.entries[index];
            let from = if index == start.0 { start.1 } else { 0 };
            let to = if index == end.0 {
                end.1
            } else {
                entry.text.len()
            };
            let from = clamp_boundary(&entry.text, from);
            let to = clamp_boundary(&entry.text, to);
            // A fully crossed empty code line carries no glyph range to wash,
            // but keeping its span preserves the blank line when copying.
            let crossed_empty = entry.text.is_empty() && index > start.0 && index < end.0;
            if from < to || crossed_empty {
                spans.push(Span {
                    key: entry.key.clone(),
                    range: from..to,
                    text: entry.text.clone(),
                    block_break: entry.block_break && !spans.is_empty(),
                });
            }
        }
        spans
    }
}

/// Clamp a byte offset into `text` and snap it down to a char boundary. Mouse
/// hit-testing lands on boundaries already; this guards the arithmetic paths.
fn clamp_boundary(text: &str, offset: usize) -> usize {
    let mut offset = offset.min(text.len());
    while offset > 0 && !text.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

/// Shared handles the renderer clones into paint closures.
pub struct SelectionState<G = ()> {
    pub selection: Rc<RefCell<Selection>>,
    pub registry: Rc<RefCell<SelectionRegistry<G>>>,
}

impl<G> Clone for SelectionState<G> {
    fn clone(&self) -> Self {
        Self {
            selection: self.selection.clone(),
            registry: self.registry.clone(),
        }
    }
}

impl<G> Default for SelectionState<G> {
    fn default() -> Self {
        Self {
            selection: Rc::default(),
            registry: Rc::default(),
        }
    }
}

impl<G> SelectionState<G> {
    /// Drop both the persisted spans and this frame's hit-test geometry.
    pub fn clear(&self) {
        self.selection.borrow_mut().clear();
        self.registry.borrow_mut().clear();
    }
}

/// Byte range around `offset` for a double click: a word-ish run, or the
/// single non-space character under the cursor.
pub fn word_range(text: &str, offset: usize) -> Range<usize> {
    let offset = clamp_boundary(text, offset);
    let is_word = |ch: char| ch.is_alphanumeric() || ch == '_';
    let before = text[..offset].chars().next_back();
    let at = text[offset..].chars().next();

    if !at.is_some_and(is_word) && !before.is_some_and(is_word) {
        return match at {
            Some(ch) if !ch.is_whitespace() => offset..offset + ch.len_utf8(),
            _ => offset..offset,
        };
    }
    let start = text[..offset]
        .char_indices()
        .rev()
        .take_while(|(_, ch)| is_word(*ch))
        .last()
        .map_or(offset, |(index, _)| index);
    let end = text[offset..]
        .char_indices()
        .take_while(|(_, ch)| is_word(*ch))
        .last()
        .map_or(offset, |(index, ch)| offset + index + ch.len_utf8());
    start..end
}

/// Byte range of the logical line containing `offset`, for a triple click.
pub fn line_range(text: &str, offset: usize) -> Range<usize> {
    let offset = clamp_boundary(text, offset);
    let start = text[..offset].rfind('\n').map_or(0, |newline| newline + 1);
    let end = text[offset..]
        .find('\n')
        .map_or(text.len(), |newline| offset + newline);
    start..end
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry(entries: &[(&str, &str)]) -> SelectionRegistry {
        // Geometry defaults to `()` here: resolve() is pure index arithmetic.
        let mut registry = SelectionRegistry::default();
        for (index, (row, text)) in entries.iter().enumerate() {
            registry.push(RegisteredText {
                key: TextKey::new(*row, index),
                text: Rc::from(*text),
                block_break: index > 0,
                geometry: (),
            });
        }
        registry
    }

    fn sample() -> SelectionRegistry {
        registry(&[
            ("r1", "first paragraph"),
            ("r1", "second"),
            ("r2", "third one"),
        ])
    }

    fn selected(registry: &SelectionRegistry, spans: &[Span]) -> Vec<String> {
        let _ = registry;
        spans
            .iter()
            .map(|span| span.text[span.range.clone()].to_owned())
            .collect()
    }

    #[test]
    fn resolves_within_one_element() {
        let registry = sample();
        let spans = registry.resolve((0, 6), (0, 15));
        assert_eq!(selected(&registry, &spans), vec!["paragraph"]);
        // Direction does not matter.
        assert_eq!(registry.resolve((0, 15), (0, 6)), spans);
    }

    #[test]
    fn resolves_across_elements_covering_middles_whole() {
        let registry = sample();
        let spans = registry.resolve((0, 6), (2, 5));
        assert_eq!(
            selected(&registry, &spans),
            vec!["paragraph", "second", "third"]
        );
        // A bottom-up drag resolves identically.
        assert_eq!(registry.resolve((2, 5), (0, 6)), spans);
    }

    #[test]
    fn resolve_clamps_offsets_and_indexes() {
        let registry = sample();
        let spans = registry.resolve((0, 0), (99, 999));
        assert_eq!(
            selected(&registry, &spans),
            vec!["first paragraph", "second", "third one"]
        );
    }

    #[test]
    fn resolve_snaps_offsets_to_char_boundaries() {
        let registry = registry(&[("r1", "héllo")]);
        // Byte 2 is inside the 'é'; snapping down keeps the slice valid.
        let spans = registry.resolve((0, 2), (0, 5));
        assert_eq!(spans[0].range.start, 1);
        assert!(!spans.is_empty());
    }

    #[test]
    fn drag_lifecycle_and_copy_joining() {
        let registry = sample();
        let mut selection = Selection::default();
        let anchor = TextKey::new("r1", 0);

        selection.begin(anchor.clone(), 6);
        assert_eq!(selection.drag_anchor(&anchor), Some(6));
        assert_eq!(selection.drag_anchor(&TextKey::new("r1", 1)), None);

        let spans = registry.resolve((0, 6), (1, 6));
        assert!(selection.set_spans(spans.clone()));
        // An unchanged resolve must not force a repaint.
        assert!(!selection.set_spans(spans));

        assert_eq!(selection.wash_range(&anchor), Some(6..15));
        assert_eq!(selection.wash_range(&TextKey::new("r1", 1)), Some(0..6));
        assert_eq!(selection.wash_range(&TextKey::new("r2", 2)), None);

        // Elements starting a new block join with a paragraph break.
        assert_eq!(
            selection.end_drag(&anchor).as_deref(),
            Some("paragraph\n\nsecond")
        );
        assert_eq!(selection.text(), "paragraph\n\nsecond");

        // A mouse-down outside every text element clears the settled selection.
        selection.clear();
        assert!(selection.is_empty());
        assert_eq!(selection.anchor(), None);
    }

    #[test]
    fn line_oriented_copy_uses_single_newlines_and_preserves_blank_rows() {
        let registry = registry(&[("line-1", "one"), ("line-2", ""), ("line-3", "two")]);
        let mut selection = Selection::default();
        selection.set_spans(registry.resolve((0, 0), (2, 3)));

        // The fixture marks later elements as block starts; Review overrides
        // that flag because every registered element is one logical code line.
        for span in &mut selection.spans {
            span.block_break = false;
        }
        assert_eq!(selection.text(), "one\n\ntwo");
    }

    #[test]
    fn a_click_without_movement_clears_on_release() {
        let mut selection = Selection::default();
        let key = TextKey::new("r1", 0);
        selection.begin(key.clone(), 3);
        assert_eq!(selection.end_drag(&key), None);
        assert!(selection.is_empty());
        assert_eq!(selection.anchor(), None);
    }

    #[test]
    fn double_and_triple_click_spans() {
        let mut selection = Selection::default();
        let key = TextKey::new("r1", 0);
        let text: Rc<str> = Rc::from("hello world");
        selection.begin_with_span(key.clone(), text.clone(), 6..11);
        assert_eq!(selection.wash_range(&key), Some(6..11));
        assert_eq!(selection.end_drag(&key).as_deref(), Some("world"));
    }

    #[test]
    fn word_ranges_follow_word_characters() {
        let text = "let foo_bar = 12;";
        assert_eq!(word_range(text, 5), 4..11);
        assert_eq!(word_range(text, 4), 4..11);
        assert_eq!(word_range(text, 11), 4..11);
        assert_eq!(word_range(text, 15), 14..16);
        assert_eq!(&text[word_range(text, 12)], "=");
        assert_eq!(word_range(text, 3), 0..3);

        // Mid-character offsets snap down rather than panicking.
        let unicode = "héllo wörld";
        assert_eq!(&unicode[word_range(unicode, 2)], "héllo");
    }

    #[test]
    fn line_ranges_cover_the_surrounding_logical_line() {
        let text = "first line\nsecond line\nthird";
        assert_eq!(line_range(text, 3), 0..10);
        assert_eq!(line_range(text, 15), 11..22);
        assert_eq!(line_range(text, text.len()), 23..28);
    }

    #[test]
    fn registry_positions_survive_rebuilds() {
        let mut registry = SelectionRegistry::default();
        registry.push(RegisteredText {
            key: TextKey::new("row-a", 0),
            text: Rc::from("a"),
            block_break: false,
            geometry: (),
        });
        assert_eq!(registry.position(&TextKey::new("row-a", 0)), Some(0));
        assert_eq!(registry.position(&TextKey::new("row-b", 0)), None);
        registry.clear();
        assert!(registry.is_empty());
        assert_eq!(registry.position(&TextKey::new("row-a", 0)), None);
    }
}

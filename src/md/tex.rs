//! LaTeX-style math delimiters, normalized before the markdown parser runs.
//!
//! `pulldown-cmark`'s math extension only knows `$…$` and `$$…$$`. A model
//! that writes the LaTeX-style `\[…\]` and `\(…\)` instead has its
//! backslashes eaten by CommonMark's ASCII-punctuation escape, so the
//! delimiters are gone before the math extension ever sees them and the
//! formula renders as mangled text — `[` and `(` with the body left intact,
//! because letters are not escapable.
//!
//! This pass finds those delimiters outside code and rewrites them to the
//! dollar form. The formula is taken from the source verbatim, so escapes
//! inside it (`\{`, `\%`) survive the trip.

use std::borrow::Cow;
use std::ops::Range;

/// One LaTeX-style math region.
pub struct TexRegion {
    /// The region in the rewritten text.
    rewritten: Range<usize>,
    /// The formula, verbatim from the source.
    latex: String,
    /// Display math (`\[…\]`) rather than inline (`\(…\)`).
    display: bool,
}

/// A source with its LaTeX-style math delimiters rewritten to dollars.
pub struct TexRewrite<'a> {
    /// The rewritten source, borrowed unchanged when there is nothing to do.
    pub text: Cow<'a, str>,
    regions: Vec<TexRegion>,
}

impl<'a> TexRewrite<'a> {
    /// Rewrite the LaTeX-style math delimiters in `source`.
    pub fn new(source: &'a str) -> Self {
        let regions = find_regions(source);
        let (text, regions) = build(source, regions);
        Self { text, regions }
    }

    /// Map an offset in the rewritten text back to the source.
    ///
    /// An inline region loses the two bytes its backslashes took up, so every
    /// offset after one sits that much earlier in the rewritten text.
    pub fn source_offset(&self, offset: usize) -> usize {
        let mut shift = 0;
        for region in &self.regions {
            if !region.display && region.rewritten.end <= offset {
                shift += 2;
            }
        }
        offset + shift
    }

    /// The range of a block in the source.
    pub fn source_range(&self, range: &Range<usize>) -> Range<usize> {
        self.source_offset(range.start)..self.source_offset(range.end)
    }

    /// The formula of the math event at `range`, when it came from a region
    /// this pass rewrote. A formula in dollars keeps the parser's own text.
    pub fn latex_at(&self, range: &Range<usize>) -> Option<&str> {
        self.regions
            .iter()
            .find(|region| &region.rewritten == range)
            .map(|region| region.latex.as_str())
    }
}

/// A math region found in the source.
struct Found {
    /// Byte range of the whole region, delimiters included.
    range: Range<usize>,
    /// Display math (`\[…\]`) rather than inline (`\(…\)`).
    display: bool,
}

/// Find the LaTeX-style math regions in `source`.
///
/// Code spans and fenced code blocks are skipped: a `\[` in code is literal
/// text, and rewriting it would change what the code says.
fn find_regions(source: &str) -> Vec<Found> {
    let bytes = source.as_bytes();
    let mut regions = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        // A code fence opens at the start of a line.
        if at_line_start(bytes, index)
            && let Some((marker, run)) = fence_at(bytes, index)
        {
            index = skip_fence(bytes, index, marker, run);
            continue;
        }
        match bytes[index] {
            b'`' => index = skip_code_span(bytes, index),
            b'\\' => {
                let (closer, display) = match bytes.get(index + 1) {
                    Some(b'\\') => {
                        // An escaped backslash is not the start of anything.
                        index += 2;
                        continue;
                    }
                    Some(b'[') => (b']', true),
                    Some(b'(') => (b')', false),
                    _ => {
                        index += 1;
                        continue;
                    }
                };
                let Some(close) = find_closer(bytes, index + 2, closer) else {
                    // An unclosed delimiter is ordinary text: the formula is
                    // still streaming, and rewriting it would make the
                    // rendering of this block change when the closer arrives.
                    index += 1;
                    continue;
                };
                let end = close + 2;
                let body = &source[index + 2..close];
                // A dollar in the body would end the math span early, and an
                // empty body is not a formula at all.
                if body.is_empty() || body.contains('$') {
                    index = end;
                    continue;
                }
                regions.push(Found {
                    range: index..end,
                    display,
                });
                index = end;
            }
            _ => index += 1,
        }
    }
    regions
}

/// Rewrite the delimiters of every region and collect the formulas.
fn build<'a>(source: &'a str, found: Vec<Found>) -> (Cow<'a, str>, Vec<TexRegion>) {
    if found.is_empty() {
        return (Cow::Borrowed(source), Vec::new());
    }
    let mut text = String::with_capacity(source.len());
    let mut regions = Vec::with_capacity(found.len());
    let mut cursor = 0;
    for region in found {
        let start = region.range.start;
        let end = region.range.end;
        let body = &source[start + 2..end - 2];
        text.push_str(&source[cursor..start]);
        let rewritten_start = text.len();
        if region.display {
            text.push_str("$$");
            text.push_str(body);
            text.push_str("$$");
        } else {
            text.push('$');
            text.push_str(body);
            text.push('$');
        }
        regions.push(TexRegion {
            rewritten: rewritten_start..text.len(),
            latex: body.to_owned(),
            display: region.display,
        });
        cursor = end;
    }
    text.push_str(&source[cursor..]);
    (text.into(), regions)
}

/// Find the closing delimiter of a math region.
///
/// Returns the offset of the backslash that opens the closer. A blank line
/// ends the search: a formula cannot span two paragraphs, and a region that
/// spans one must not change the rendering of an earlier block when its
/// closer finally arrives.
fn find_closer(bytes: &[u8], from: usize, closer: u8) -> Option<usize> {
    let mut index = from;
    while index < bytes.len() {
        match bytes[index] {
            b'\n' => {
                let mut next = index + 1;
                while next < bytes.len() && (bytes[next] == b' ' || bytes[next] == b'\t') {
                    next += 1;
                }
                if next < bytes.len() && bytes[next] == b'\n' {
                    return None;
                }
                index += 1;
            }
            b'`' => index = skip_code_span(bytes, index),
            b'\\' => {
                if bytes.get(index + 1) == Some(&closer) {
                    return Some(index);
                }
                index += 2;
            }
            _ => index += 1,
        }
    }
    None
}

/// Skip an inline code span, returning the offset just past it.
///
/// A backtick run with no matching run is literal text, so the scan carries on
/// after the run instead of swallowing the rest of the source.
fn skip_code_span(bytes: &[u8], start: usize) -> usize {
    let run = bytes[start..]
        .iter()
        .take_while(|&&byte| byte == b'`')
        .count();
    let mut index = start + run;
    while index < bytes.len() {
        if bytes[index] != b'`' {
            index += 1;
            continue;
        }
        let other = bytes[index..]
            .iter()
            .take_while(|&&byte| byte == b'`')
            .count();
        if other == run {
            return index + other;
        }
        index += other;
    }
    start + run
}

/// The fence that opens at `line_start`, as its marker and run length.
fn fence_at(bytes: &[u8], line_start: usize) -> Option<(u8, usize)> {
    let mut cursor = line_start;
    let mut spaces = 0;
    while cursor < bytes.len() && bytes[cursor] == b' ' && spaces < 4 {
        cursor += 1;
        spaces += 1;
    }
    if spaces > 3 {
        return None;
    }
    let marker = *bytes.get(cursor)?;
    if marker != b'`' && marker != b'~' {
        return None;
    }
    let run = bytes[cursor..]
        .iter()
        .take_while(|&&byte| byte == marker)
        .count();
    (run >= 3).then_some((marker, run))
}

/// Skip a fenced code block, returning the offset just past its closing fence.
fn skip_fence(bytes: &[u8], start: usize, marker: u8, open_run: usize) -> usize {
    let mut index = start;
    loop {
        while index < bytes.len() && bytes[index] != b'\n' {
            index += 1;
        }
        if index >= bytes.len() {
            return bytes.len();
        }
        index += 1;
        let mut cursor = index;
        while cursor < bytes.len() && bytes[cursor] == b' ' {
            cursor += 1;
        }
        let run = bytes[cursor..]
            .iter()
            .take_while(|&&byte| byte == marker)
            .count();
        if run >= open_run {
            while cursor < bytes.len() && bytes[cursor] != b'\n' {
                cursor += 1;
            }
            return cursor;
        }
    }
}

/// Whether `index` starts a line.
fn at_line_start(bytes: &[u8], index: usize) -> bool {
    index == 0 || bytes[index - 1] == b'\n'
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrites_display_and_inline_regions() {
        let source = r"\[x^2\] and \(y\)";
        let rewrite = TexRewrite::new(source);
        assert_eq!(rewrite.text, "$$x^2$$ and $y$");
        assert_eq!(rewrite.regions.len(), 2);
        assert!(rewrite.regions[0].display);
        assert!(!rewrite.regions[1].display);
        for region in &rewrite.regions {
            let source_range = rewrite.source_range(&region.rewritten);
            assert_eq!(
                &source[source_range],
                if region.display { r"\[x^2\]" } else { r"\(y\)" }
            );
        }
        assert_eq!(rewrite.latex_at(&rewrite.regions[0].rewritten), Some("x^2"));
        assert_eq!(rewrite.latex_at(&rewrite.regions[1].rewritten), Some("y"));
    }

    #[test]
    fn leaves_code_and_unfinished_math_alone() {
        for source in [
            r"`\[x\]`",
            "```rust\n\\[x\\]\n```",
            "~~~\n\\(x\\)\n~~~",
            r"unclosed \[\frac{a}{b}",
            r"\(",
            r"\[\]",
            r"\[$x$\]",
            // An escaped backslash before a bracket is a literal backslash,
            // and LaTeX's `\\[…]` line-break spacing is not a delimiter.
            r"\\[not math\]",
        ] {
            let rewrite = TexRewrite::new(source);
            assert_eq!(rewrite.text, source, "{source}");
            assert!(rewrite.regions.is_empty(), "{source}");
        }
    }

    #[test]
    fn a_formula_cannot_span_two_paragraphs() {
        let source = "\\[x\n\ny\\]";
        let rewrite = TexRewrite::new(source);
        assert_eq!(rewrite.text, source);
    }

    #[test]
    fn a_closer_inside_code_does_not_close_the_formula() {
        let source = "\\(a `\\)` b\\)";
        let rewrite = TexRewrite::new(source);
        assert_eq!(rewrite.text, "$a `\\)` b$");
    }

    #[test]
    fn latex_line_break_spacing_is_not_a_delimiter() {
        // `\\[2em]` is a line break with spacing, inside a formula.
        let source = r"\[x = y \\[2em] z = w\]";
        let rewrite = TexRewrite::new(source);
        assert_eq!(rewrite.text, "$$x = y \\\\[2em] z = w$$");
        assert_eq!(
            rewrite.latex_at(&rewrite.regions[0].rewritten),
            Some("x = y \\\\[2em] z = w")
        );
    }

    #[test]
    fn maps_offsets_back_to_the_source() {
        let source = "a \\(x\\) b \\(y\\) c";
        let rewrite = TexRewrite::new(source);
        assert_eq!(rewrite.text, "a $x$ b $y$ c");
        assert_eq!(rewrite.source_offset(rewrite.text.len()), source.len());
        // The first region shifts only what follows the second one.
        assert_eq!(&source[rewrite.source_range(&(0..2))], "a ");
        assert_eq!(&source[rewrite.source_range(&(6..7))], "b");
        assert_eq!(
            &source[rewrite.source_range(&(rewrite.text.len() - 1..rewrite.text.len()))],
            "c"
        );
    }
}

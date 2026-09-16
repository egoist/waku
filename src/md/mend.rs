//! Closes hanging inline markers in a *streaming* markdown tail.
//!
//! While a paragraph streams, an unclosed `**bold`, `*em`, `` `code ``,
//! `~~strike` or `[text](half-url` parses as literal text. When the closer
//! finally arrives the marker characters vanish and the run restyles, so wrap
//! points shift and the paragraph's tail visibly reflows — a jitter that lands
//! on every emphasis in a streamed response.
//!
//! Appending synthetic closers to the *display* parse keeps styling stable from
//! the moment content follows an opener. Only the display tree sees mended text
//! ([`super::parser::IncrementalParser::display_tree`]); the canonical tree is
//! untouched, so a marker that genuinely never closes settles honestly — one
//! flip when the response completes — instead of jittering throughout.
//!
//! The scanner is deliberately approximate: it prefers "stable and close to the
//! final parse" over exact CommonMark delimiter resolution, because any
//! mid-stream misjudgement is repaired by the very next append or by the
//! settle. Cost is one pass over the tail, and `None` — zero further work —
//! whenever nothing hangs, which is the overwhelmingly common case.

/// Destination for a link whose URL is still streaming. The renderer styles it
/// like a link but must not make it clickable.
pub const PENDING_LINK_URL: &str = "waku:pending-link";

/// Zero-width space appended to defuse a would-be setext underline.
const ZERO_WIDTH_SPACE: char = '\u{200B}';

/// One unclosed emphasis-family delimiter run.
#[derive(Debug)]
struct OpenDelimiter {
    marker: char,
    /// Closers still owed. A short closer (`**a*`) decrements this.
    owed: usize,
    /// Char index just past the opening run: both the nesting order and the
    /// "content must follow" guard compare against it.
    opened_at: usize,
}

/// Repair hanging inline markers. Returns `None` when nothing needs repair.
pub fn close_hanging(text: &str) -> Option<String> {
    let chars = text.char_indices().collect::<Vec<_>>();
    let at = |index: usize| chars.get(index).map(|&(_, ch)| ch);

    let mut delimiters: Vec<OpenDelimiter> = Vec::new();
    let mut brackets: Vec<usize> = Vec::new();
    // Open inline code span: (backtick run length, char index of its content).
    let mut code: Option<(usize, usize)> = None;
    let mut last_content: Option<usize> = None;
    // Char index of the `]` whose `](…` URL runs off the end of the tail.
    let mut pending_url: Option<usize> = None;

    let mut index = 0;
    while index < chars.len() {
        let ch = chars[index].1;

        if code.is_none() && ch == '\\' {
            // An escape and its escapee are both literal, but the escapee is
            // still content that can justify closing an opener.
            if index + 1 < chars.len() {
                last_content = Some(index + 1);
            }
            index += 2;
            continue;
        }

        if ch == '`' {
            let run = run_length(&chars, index);
            match code {
                // A span closes only on a run of exactly the opening length.
                // Its closing backticks are content: an emphasis closer owed
                // from before the span must land after them, not inside it.
                Some((open, _)) if run == open => {
                    code = None;
                    last_content = Some(index + run - 1);
                }
                Some(_) => last_content = Some(index + run - 1),
                None => code = Some((run, index + run)),
            }
            index += run;
            continue;
        }

        if code.is_some() {
            last_content = Some(index);
            index += 1;
            continue;
        }

        if ch == '$' {
            // TeX brackets, underscores and backticks are not Markdown
            // delimiters. Never synthesize a closer inside unfinished math.
            let run = run_length(&chars, index).min(2);
            let mut scan = index + run;
            let mut closed = false;
            while scan < chars.len() {
                if at(scan) == Some('\\') {
                    scan += 2;
                    continue;
                }
                if at(scan) == Some('$') && run_length(&chars, scan) >= run {
                    index = scan + run;
                    last_content = Some(index - 1);
                    closed = true;
                    break;
                }
                scan += 1;
            }
            if closed {
                continue;
            }
            if run == 2
                || at(index + 1).is_some_and(|ch| !ch.is_whitespace() && !ch.is_ascii_digit())
            {
                return None;
            }
        }

        match ch {
            '*' | '_' | '~' => {
                let run = run_length(&chars, index);
                scan_delimiter(&mut delimiters, ch, run, index, &mut last_content, &at);
                index += run;
            }
            '[' => {
                brackets.push(index);
                index += 1;
            }
            ']' => {
                if let Some(open) = brackets.pop() {
                    // Emphasis opened inside a completed `[…]` and never closed
                    // there stays literal, as the final parse will also decide.
                    delimiters.retain(|delimiter| delimiter.opened_at < open);
                    if at(index + 1) == Some('(') {
                        // Consume the URL through its balanced `)`.
                        let mut scan = index + 2;
                        let mut depth = 0usize;
                        loop {
                            match at(scan) {
                                Some('(') => depth += 1,
                                Some(')') if depth == 0 => break,
                                Some(')') => depth -= 1,
                                Some(_) => {}
                                None => {
                                    pending_url = Some(index);
                                    break;
                                }
                            }
                            scan += 1;
                        }
                        if pending_url.is_some() {
                            break;
                        }
                        last_content = Some(scan);
                        index = scan + 1;
                        continue;
                    }
                }
                last_content = Some(index);
                index += 1;
            }
            ch if ch.is_whitespace() => index += 1,
            _ => {
                last_content = Some(index);
                index += 1;
            }
        }
    }

    // Closers must sit immediately after the last content character: a `**`
    // preceded by whitespace is left-flanking, so it would open a *new* run
    // rather than close the hanging one, and the repair would never converge.
    let content_end = last_content
        .map(|index| chars[index].0 + chars[index].1.len_utf8())
        .unwrap_or(text.len());

    if let Some(bracket) = pending_url {
        // The URL never renders, so a bare `[text` and a half-typed
        // `[text](http` mend identically: the link text shows styled at once
        // and the settling URL cannot collapse the line.
        let cut = chars[bracket].0;
        let mut closers = String::new();
        close_delimiters(&mut closers, &delimiters, last_content, bracket);
        let mut mended = String::with_capacity(cut + closers.len() + PENDING_LINK_URL.len() + 4);
        mended.push_str(&text[..content_end.min(cut)]);
        mended.push_str(&closers);
        mended.push_str(&text[content_end.min(cut)..cut]);
        mended.push_str("](");
        mended.push_str(PENDING_LINK_URL);
        mended.push(')');
        return Some(mended);
    }

    let mut closers = String::new();
    if let Some((run, content_at)) = code
        && last_content.is_some_and(|content| content >= content_at)
    {
        closers.extend(std::iter::repeat_n('`', run));
    }
    close_delimiters(&mut closers, &delimiters, last_content, chars.len());
    if let Some(bracket) = brackets.last().copied()
        && last_content.is_some_and(|content| content > bracket)
    {
        closers.push_str("](");
        closers.push_str(PENDING_LINK_URL);
        closers.push(')');
    }

    let setext_guard = needs_setext_guard(text);
    if closers.is_empty() && !setext_guard {
        return None;
    }

    let mut mended = String::with_capacity(text.len() + closers.len() + 3);
    mended.push_str(&text[..content_end]);
    mended.push_str(&closers);
    mended.push_str(&text[content_end..]);
    if setext_guard {
        mended.push(ZERO_WIDTH_SPACE);
    }
    Some(mended)
}

/// Append closers for every still-open delimiter, innermost first, skipping any
/// opener that has no content after it yet (`**` alone must stay literal, not
/// become an empty bold).
fn close_delimiters(
    out: &mut String,
    delimiters: &[OpenDelimiter],
    last_content: Option<usize>,
    limit: usize,
) {
    for delimiter in delimiters.iter().rev() {
        if delimiter.opened_at >= limit {
            continue;
        }
        let has_content = last_content.is_some_and(|content| content >= delimiter.opened_at);
        if !has_content {
            continue;
        }
        out.extend(std::iter::repeat_n(delimiter.marker, delimiter.owed));
    }
}

fn run_length(chars: &[(usize, char)], start: usize) -> usize {
    let marker = chars[start].1;
    chars[start..]
        .iter()
        .take_while(|&&(_, ch)| ch == marker)
        .count()
}

/// Classify one emphasis-family run as an opener or a closer and update the
/// open-delimiter stack.
fn scan_delimiter(
    delimiters: &mut Vec<OpenDelimiter>,
    marker: char,
    run: usize,
    index: usize,
    last_content: &mut Option<usize>,
    at: &impl Fn(usize) -> Option<char>,
) {
    // A lone `~` is literal in GFM: strikethrough needs `~~`.
    if marker == '~' && run < 2 {
        *last_content = Some(index + run - 1);
        return;
    }

    let before = index.checked_sub(1).and_then(at);
    let after = at(index + run);
    let can_close = before.is_some_and(|ch| !ch.is_whitespace());
    let can_open = after.is_some_and(|ch| !ch.is_whitespace());

    if can_close
        && let Some(position) = delimiters
            .iter()
            .rposition(|delimiter| delimiter.marker == marker)
        && last_content.is_some_and(|content| content >= delimiters[position].opened_at)
    {
        let owed = delimiters[position].owed;
        if run >= owed {
            // Fully closed; any surplus markers are rare enough to ignore.
            delimiters.truncate(position);
        } else {
            // A half-streamed closer (`**a*`): we still owe the remainder.
            delimiters[position].owed = owed - run;
            delimiters.truncate(position + 1);
        }
        *last_content = Some(index + run - 1);
        return;
    }

    // `_` does not open inside a word, matching CommonMark's intraword rule.
    if marker == '_' && before.is_some_and(char::is_alphanumeric) {
        *last_content = Some(index + run - 1);
        return;
    }

    if can_open {
        delimiters.push(OpenDelimiter {
            marker,
            owed: run,
            opened_at: index + run,
        });
    } else {
        *last_content = Some(index + run - 1);
    }
}

/// A final line of only `-` or `=` directly under a text line is a setext
/// heading underline, so a streaming list item flashes its whole paragraph as a
/// heading for one chunk. True when that flash is imminent.
fn needs_setext_guard(text: &str) -> bool {
    if text.ends_with('\n') {
        return false;
    }
    let mut lines = text.lines().rev();
    let Some(last) = lines.next() else {
        return false;
    };
    let trimmed = last.trim_end();
    if trimmed.is_empty()
        || !(trimmed.chars().all(|ch| ch == '-') || trimmed.chars().all(|ch| ch == '='))
    {
        return false;
    }
    lines.next().is_some_and(|previous| {
        let previous = previous.trim();
        !previous.is_empty() && !previous.starts_with(['-', '=', '#', '>', '`'])
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settled_text_needs_no_repair() {
        for text in [
            "plain text",
            "**bold** and *em* and `code`",
            "~~struck~~ through",
            "[link](https://example.com) done",
            "a * b * c",
            "",
        ] {
            assert_eq!(close_hanging(text), None, "unexpected repair for {text:?}");
        }
    }

    #[test]
    fn closes_hanging_emphasis() {
        assert_eq!(close_hanging("now **bold").as_deref(), Some("now **bold**"));
        assert_eq!(close_hanging("an *em").as_deref(), Some("an *em*"));
        assert_eq!(close_hanging("an _em").as_deref(), Some("an _em_"));
        assert_eq!(close_hanging("__strong").as_deref(), Some("__strong__"));
        assert_eq!(close_hanging("~~struck").as_deref(), Some("~~struck~~"));
        assert_eq!(
            close_hanging("It costs $5, **cheap").as_deref(),
            Some("It costs $5, **cheap**")
        );
    }

    #[test]
    fn closes_nested_emphasis_innermost_first() {
        assert_eq!(
            close_hanging("**bold *both").as_deref(),
            Some("**bold *both***")
        );
    }

    #[test]
    fn completes_a_half_streamed_closer() {
        assert_eq!(close_hanging("**bold*").as_deref(), Some("**bold**"));
    }

    #[test]
    fn openers_without_content_stay_literal() {
        // Nothing to style yet, so the markers must remain as typed.
        assert_eq!(close_hanging("text **"), None);
        assert_eq!(close_hanging("text ** "), None);
    }

    #[test]
    fn closes_hanging_inline_code() {
        assert_eq!(close_hanging("call `foo").as_deref(), Some("call `foo`"));
        assert_eq!(close_hanging("call ``a`b").as_deref(), Some("call ``a`b``"));
        // A bare opener has no content to style yet.
        assert_eq!(close_hanging("call `"), None);
    }

    #[test]
    fn emphasis_inside_inline_code_is_literal() {
        assert_eq!(close_hanging("`a ** b`"), None);
    }

    #[test]
    fn mends_links_whose_url_is_still_streaming() {
        assert_eq!(
            close_hanging("see [docs](https://exa").as_deref(),
            Some(&format!("see [docs]({PENDING_LINK_URL})")[..])
        );
        assert_eq!(
            close_hanging("see [docs").as_deref(),
            Some(&format!("see [docs]({PENDING_LINK_URL})")[..])
        );
        // A bare `[` has no link text yet.
        assert_eq!(close_hanging("see ["), None);
    }

    #[test]
    fn emphasis_inside_a_streaming_link_text_closes_first() {
        assert_eq!(
            close_hanging("see [**docs").as_deref(),
            Some(&format!("see [**docs**]({PENDING_LINK_URL})")[..])
        );
    }

    #[test]
    fn escapes_keep_markers_literal() {
        assert_eq!(close_hanging(r"literal \*star"), None);
    }

    #[test]
    fn defuses_a_streaming_setext_underline() {
        let mended = close_hanging("paragraph\n-").expect("guard should apply");
        assert_eq!(mended, format!("paragraph\n-{ZERO_WIDTH_SPACE}"));
        // A blank line between means it is a list bullet, not an underline.
        assert_eq!(close_hanging("paragraph\n\n-"), None);
        // A settled line break is already unambiguous.
        assert_eq!(close_hanging("paragraph\n-\n"), None);
    }

    /// Streaming any prefix must never panic and must stay valid UTF-8 — the
    /// scanner runs on every delta, so robustness matters more than precision.
    #[test]
    fn every_prefix_of_a_corpus_mends_without_panicking() {
        let corpus = "Mixed **bold `code`** and *em*, a [link](https://example.com), \
                      ~~struck~~, emoji 🎉, accents héllo, and a list:\n\n- one\n- two\n";
        for end in 0..=corpus.len() {
            if !corpus.is_char_boundary(end) {
                continue;
            }
            let prefix = &corpus[..end];
            if let Some(mended) = close_hanging(prefix) {
                // Repair only ever inserts markers, except for a streaming
                // link whose half-typed URL is swapped for the sentinel.
                assert!(
                    mended.len() >= prefix.len()
                        || mended.ends_with(&format!("({PENDING_LINK_URL})")),
                    "mending {prefix:?} shrank it to {mended:?}"
                );
                // Idempotence is the convergence guarantee: one pass must
                // leave nothing hanging, or a streamed response would grow
                // markers on every delta.
                assert_eq!(
                    close_hanging(&mended),
                    None,
                    "mending {prefix:?} into {mended:?} left work behind"
                );
            }
        }
    }
}

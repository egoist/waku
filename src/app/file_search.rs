//! VS Code-style find and replace for the right panel's file editor.
//!
//! One find bar serves whichever file editor is on screen. The bar owns the
//! query, its toggles, and the computed match list; the editor entity only
//! paints the washes it is handed. Matches are recomputed on one-shot events —
//! typing in the find input, editing the file, switching files — never per
//! frame.

use regex::Regex;

use super::*;

/// Where a match scan stops. Everything under the cap is highlighted and
/// navigable; a file with more occurrences than this reports a `+` count.
/// Replace All ignores the cap and rescans, so it never misses the tail.
const MAX_FILE_SEARCH_MATCHES: usize = 20_000;

/// Horizontal space the find row's fixed controls take beside the query
/// input: the replace chevron, match count, divider, navigation and close
/// buttons, paddings and gaps. The input gets whatever the pane leaves over.
const FIND_BAR_CONTROLS_WIDTH: f32 = 252.0;

pub(super) struct FileSearch {
    open: bool,
    /// The file the current match list was computed against. Cleared to an
    /// empty string when the list must not be trusted, e.g. after a session
    /// switch swapped every editor out from under it.
    path: String,
    query: Entity<TextInput>,
    replace: Entity<TextInput>,
    replace_visible: bool,
    case_sensitive: bool,
    whole_word: bool,
    use_regex: bool,
    /// Sorted, non-overlapping, and capped at [`MAX_FILE_SEARCH_MATCHES`].
    matches: Vec<Range<usize>>,
    /// Index into `matches` navigation is on. `Some` whenever `matches` is
    /// non-empty, so the count always reads "n of m".
    current: Option<usize>,
    /// The scan stopped at the cap; the count shows `m+`.
    limited: bool,
    /// The query is a regular expression that does not compile.
    invalid: bool,
}

/// What triggered a match recomputation, which decides how the current match
/// re-anchors and whether the editor's selection and scroll follow it.
enum SearchRefresh {
    /// The query or a toggle changed: re-anchor at the editor's selection,
    /// select the match and scroll to it.
    Query,
    /// The file's content changed under an open search: keep the current
    /// match by position, touch neither selection nor scroll.
    Content,
    /// Another file became the visible editor: recompute against it without
    /// selecting or scrolling.
    Target(String),
}

/// The compiled query, or why there is nothing to run.
enum CompiledSearch {
    Empty,
    Invalid,
    Ready(Regex),
}

fn compile_search(
    query: &str,
    case_sensitive: bool,
    whole_word: bool,
    use_regex: bool,
) -> CompiledSearch {
    if query.is_empty() {
        return CompiledSearch::Empty;
    }
    let mut pattern = if use_regex {
        query.to_owned()
    } else {
        regex::escape(query)
    };
    if whole_word {
        pattern = format!(r"\b(?:{pattern})\b");
    }
    match regex::RegexBuilder::new(&pattern)
        .case_insensitive(!case_sensitive)
        .multi_line(true)
        .build()
    {
        Ok(regex) => CompiledSearch::Ready(regex),
        Err(_) => CompiledSearch::Invalid,
    }
}

/// Every non-empty match in order, stopping at `cap`. Zero-width matches (a
/// regex like `a*`) are skipped: they cannot be seen or replaced meaningfully.
fn collect_search_matches(
    content: &str,
    regex: &Regex,
    cap: Option<usize>,
) -> (Vec<Range<usize>>, bool) {
    let mut matches = Vec::new();
    for found in regex.find_iter(content) {
        if found.range().is_empty() {
            continue;
        }
        if cap.is_some_and(|cap| matches.len() >= cap) {
            return (matches, true);
        }
        matches.push(found.range());
    }
    (matches, false)
}

/// The replacement text for one match. In regex mode `$1`-style group
/// references expand; otherwise the template is inserted verbatim, so a `$`
/// in a plain replacement stays a `$`.
fn expand_replacement(
    regex: &Regex,
    content: &str,
    match_range: &Range<usize>,
    template: &str,
    use_regex: bool,
) -> String {
    if !use_regex {
        return template.to_owned();
    }
    let captures = regex
        .captures_at(content, match_range.start)
        .filter(|captures| {
            captures
                .get(0)
                .is_some_and(|whole| whole.range() == *match_range)
        });
    let Some(captures) = captures else {
        return template.to_owned();
    };
    let mut expanded = String::new();
    captures.expand(template, &mut expanded);
    expanded
}

/// The content with every listed match replaced. Splicing over the recorded
/// ranges — rather than `Regex::replace_all` — keeps "what gets replaced"
/// identical to "what was highlighted", zero-width skips included.
fn replace_all_content(
    content: &str,
    regex: &Regex,
    matches: &[Range<usize>],
    template: &str,
    use_regex: bool,
) -> String {
    let mut result = String::with_capacity(content.len());
    let mut cursor = 0;
    for match_range in matches {
        result.push_str(&content[cursor..match_range.start]);
        result.push_str(&expand_replacement(
            regex,
            content,
            match_range,
            template,
            use_regex,
        ));
        cursor = match_range.end;
    }
    result.push_str(&content[cursor..]);
    result
}

/// First match starting at or after `offset`, wrapping to the top.
fn match_at_or_after(matches: &[Range<usize>], offset: usize) -> Option<usize> {
    if matches.is_empty() {
        return None;
    }
    let index = matches.partition_point(|found| found.start < offset);
    Some(if index >= matches.len() { 0 } else { index })
}

/// Last match starting before `offset`, wrapping to the bottom.
fn match_before(matches: &[Range<usize>], offset: usize) -> Option<usize> {
    if matches.is_empty() {
        return None;
    }
    let index = matches.partition_point(|found| found.start < offset);
    Some(if index == 0 {
        matches.len() - 1
    } else {
        index - 1
    })
}

/// Where the editor's scroll offset must move for a match at `match_top`
/// (window coordinates from the last paint) to be comfortably visible, or
/// `None` if it already is. An off-screen match lands about a third of the
/// way down the viewport, the way editors conventionally reveal a find hit.
fn revealed_scroll_offset(
    current_offset: Pixels,
    max_offset: Pixels,
    match_top: Pixels,
    line_height: Pixels,
    viewport_top: Pixels,
    viewport_bottom: Pixels,
) -> Option<Pixels> {
    if viewport_bottom <= viewport_top {
        return None;
    }
    let margin = line_height;
    if match_top >= viewport_top + margin && match_top + line_height <= viewport_bottom - margin {
        return None;
    }
    let target = viewport_top + (viewport_bottom - viewport_top) * 0.35;
    let next = (current_offset + (target - match_top)).clamp(-max_offset, Pixels::ZERO);
    (next != current_offset).then_some(next)
}

impl Waku {
    pub(super) fn refresh_file_search_localized_text(&mut self, cx: &mut Context<Self>) {
        let Some(search) = &self.file_search else {
            return;
        };
        search
            .query
            .update(cx, |input, cx| input.set_placeholder(tr!("input.find"), cx));
        search.replace.update(cx, |input, cx| {
            input.set_placeholder(tr!("input.replace"), cx)
        });
    }

    fn file_search_open(&self) -> bool {
        self.file_search.as_ref().is_some_and(|search| search.open)
    }

    fn file_search_target_focused(&self, window: &Window, cx: &App) -> bool {
        if self.file_search.as_ref().is_some_and(|search| {
            search.open
                && (search.query.read(cx).focus().is_focused(window)
                    || search.replace.read(cx).focus().is_focused(window))
        }) {
            return true;
        }
        self.visible_right_panel_file_path()
            .and_then(|path| self.right_panel_file_editors.get(&path))
            .is_some_and(|editor| editor.state.read(cx).focus().is_focused(window))
    }

    /// The editor entity the open search operates on.
    fn file_search_editor(&self) -> Option<Entity<TextInput>> {
        let search = self.file_search.as_ref().filter(|search| search.open)?;
        self.right_panel_file_editors
            .get(&search.path)
            .map(|editor| editor.state.clone())
    }

    fn ensure_file_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.file_search.is_some() {
            return;
        }
        let query = cx.new(|cx| {
            TextInput::new(window, cx)
                .placeholder(tr!("input.find"))
        });
        cx.subscribe(
            &query,
            |this: &mut Self, _, event: &InputEvent, cx| match event {
                InputEvent::Edited => this.refresh_file_search(SearchRefresh::Query, cx),
                InputEvent::Submit(_) => this.file_search_navigate(false, cx),
                InputEvent::Focus => {}
                InputEvent::BackspaceOnEmpty => {}
            },
        )
        .detach();
        let replace = cx.new(|cx| {
            TextInput::new(window, cx)
                .placeholder(tr!("input.replace"))
        });
        cx.subscribe(&replace, |this: &mut Self, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Submit(_)) {
                this.file_search_replace_current(cx);
            }
        })
        .detach();
        self.file_search = Some(FileSearch {
            open: false,
            path: String::new(),
            query,
            replace,
            replace_visible: false,
            case_sensitive: false,
            whole_word: false,
            use_regex: false,
            matches: Vec::new(),
            current: None,
            limited: false,
            invalid: false,
        });
    }

    fn open_file_search(
        &mut self,
        with_replace: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.right_panel_visible {
            return;
        }
        let Some(path) = self.visible_right_panel_file_path() else {
            return;
        };
        self.ensure_file_search(window, cx);

        // Seed the query from the editor's selection, the way editors
        // conventionally do, but only a single-line one: a multi-line
        // selection means "search within", which this bar does not model.
        let seed = self.right_panel_file_editors.get(&path).and_then(|editor| {
            let editor = editor.state.read(cx);
            let selection = editor.selected_range();
            let text = editor.content().get(selection)?;
            (!text.is_empty() && !text.contains('\n')).then(|| text.to_owned())
        });

        let search = self
            .file_search
            .as_mut()
            .expect("ensure_file_search just created it");
        search.open = true;
        if with_replace {
            search.replace_visible = true;
        }
        let query = search.query.clone();
        if let Some(seed) = seed {
            query.update(cx, |query, cx| query.set_content(seed, cx));
        }
        // Arm the query for retyping and put the caret in it.
        query.update(cx, |query, cx| query.select_all_text(cx));
        window.focus(&query.read(cx).focus(), cx);

        self.refresh_file_search(SearchRefresh::Target(path), cx);
        self.refresh_file_search(SearchRefresh::Query, cx);
        cx.notify();
    }

    pub(super) fn close_file_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_file_search_with_focus(true, window, cx);
    }

    fn close_file_search_with_focus(
        &mut self,
        restore_editor_focus: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(search) = self.file_search.as_mut().filter(|search| search.open) else {
            return;
        };
        search.open = false;
        search.matches = Vec::new();
        search.current = None;
        for editor in self.right_panel_file_editors.values() {
            editor.state.update(cx, |editor, cx| {
                editor.set_search_matches(Vec::new(), None, cx)
            });
        }
        if restore_editor_focus
            && let Some(path) = self.visible_right_panel_file_path()
            && let Some(editor) = self.right_panel_file_editors.get(&path)
        {
            let focus = editor.state.read(cx).focus();
            window.focus(&focus, cx);
        }
        cx.notify();
    }

    /// Drops search state that a session switch made meaningless: the editors
    /// it pointed into were swapped out wholesale. Restored editors also get
    /// their washes cleared, in case they were stored mid-search.
    pub(super) fn reset_file_search_for_session(&mut self, cx: &mut Context<Self>) {
        if let Some(search) = self.file_search.as_mut() {
            search.open = false;
            search.path = String::new();
            search.matches = Vec::new();
            search.current = None;
        }
        for editor in self.right_panel_file_editors.values() {
            editor.state.update(cx, |editor, cx| {
                editor.set_search_matches(Vec::new(), None, cx)
            });
        }
    }

    /// Re-aims an open search at the file editor actually on screen. Called
    /// from the editor's render path: a cheap string compare per frame, and
    /// one recompute on the frame after the visible file changes.
    pub(super) fn sync_file_search_target(&mut self, relative_path: &str, cx: &mut Context<Self>) {
        if self
            .file_search
            .as_ref()
            .is_some_and(|search| search.open && search.path != relative_path)
        {
            self.refresh_file_search(SearchRefresh::Target(relative_path.to_owned()), cx);
        }
    }

    /// Follows an edit to the searched file — typing, replace, a reload from
    /// disk — keeping highlights in step with the text.
    pub(super) fn refresh_file_search_for_edit(
        &mut self,
        relative_path: &str,
        cx: &mut Context<Self>,
    ) {
        if self
            .file_search
            .as_ref()
            .is_some_and(|search| search.open && search.path == relative_path)
        {
            self.refresh_file_search(SearchRefresh::Content, cx);
        }
    }

    fn refresh_file_search(&mut self, refresh: SearchRefresh, cx: &mut Context<Self>) {
        let Some(search) = self.file_search.as_ref().filter(|search| search.open) else {
            return;
        };
        let previous_path = search.path.clone();
        let path = match &refresh {
            SearchRefresh::Target(path) => path.clone(),
            _ => previous_path.clone(),
        };
        let query_text = search.query.read(cx).content().to_owned();
        let (case_sensitive, whole_word, use_regex) =
            (search.case_sensitive, search.whole_word, search.use_regex);
        let previous_current_start = search
            .current
            .and_then(|index| search.matches.get(index))
            .map(|found| found.start);

        // The editor the search is leaving keeps no washes.
        if path != previous_path
            && let Some(editor) = self.right_panel_file_editors.get(&previous_path)
        {
            editor.state.update(cx, |editor, cx| {
                editor.set_search_matches(Vec::new(), None, cx)
            });
        }

        let editor = self
            .right_panel_file_editors
            .get(&path)
            .map(|editor| editor.state.clone());
        let (matches, limited, invalid) = match (
            &editor,
            compile_search(&query_text, case_sensitive, whole_word, use_regex),
        ) {
            (None, _) | (_, CompiledSearch::Empty) => (Vec::new(), false, false),
            (_, CompiledSearch::Invalid) => (Vec::new(), false, true),
            (Some(editor), CompiledSearch::Ready(regex)) => {
                let (matches, limited) = collect_search_matches(
                    editor.read(cx).content(),
                    &regex,
                    Some(MAX_FILE_SEARCH_MATCHES),
                );
                (matches, limited, false)
            }
        };

        let current = match &refresh {
            SearchRefresh::Query => {
                let origin = editor
                    .as_ref()
                    .map(|editor| editor.read(cx).selected_range().start)
                    .unwrap_or(0);
                match_at_or_after(&matches, origin)
            }
            SearchRefresh::Content => {
                match_at_or_after(&matches, previous_current_start.unwrap_or(0))
            }
            SearchRefresh::Target(_) => match_at_or_after(&matches, 0),
        };

        let search = self
            .file_search
            .as_mut()
            .expect("still present: nothing above removes it");
        search.path = path;
        search.matches = matches;
        search.current = current;
        search.limited = limited;
        search.invalid = invalid;

        let follow = matches!(refresh, SearchRefresh::Query);
        self.apply_file_search_to_editor(follow, follow, cx);
    }

    /// Pushes the match list into the searched editor, optionally moving its
    /// selection to the current match and scrolling it into view.
    fn apply_file_search_to_editor(&mut self, select: bool, reveal: bool, cx: &mut Context<Self>) {
        let Some(search) = self.file_search.as_ref().filter(|search| search.open) else {
            return;
        };
        let matches = search.matches.clone();
        let active = search.current;
        let active_range = active.and_then(|index| matches.get(index)).cloned();
        let Some(editor) = self.file_search_editor() else {
            cx.notify();
            return;
        };
        editor.update(cx, |editor, cx| {
            editor.set_search_matches(matches, active, cx);
            if select && let Some(range) = active_range.clone() {
                editor.select_range(range, cx);
            }
        });
        if reveal && let Some(range) = active_range {
            self.reveal_file_search_match(&editor, range.start, cx);
        }
        cx.notify();
    }

    /// Scrolls the shared editor viewport so the match at `offset` is
    /// visible. Uses the last painted layout, so right after an edit it can
    /// land a line off; the next navigation corrects it.
    fn reveal_file_search_match(
        &self,
        editor: &Entity<TextInput>,
        offset: usize,
        cx: &Context<Self>,
    ) {
        let Some((position, line_height)) = editor.read(cx).position_for_offset(offset) else {
            return;
        };
        let scroll = &self.right_panel_editor_scroll_handle;
        let viewport = scroll.bounds();
        let current = scroll.offset();
        if let Some(next) = revealed_scroll_offset(
            current.y,
            scroll.max_offset().y,
            position.y,
            line_height,
            viewport.top(),
            viewport.bottom(),
        ) {
            scroll.set_offset(point(current.x, next));
        }
    }

    fn file_search_navigate(&mut self, backwards: bool, cx: &mut Context<Self>) {
        let Some(editor) = self.file_search_editor() else {
            return;
        };
        let Some(search) = self.file_search.as_mut().filter(|search| search.open) else {
            return;
        };
        if search.matches.is_empty() {
            return;
        }
        let count = search.matches.len();
        search.current = match search.current {
            Some(current) if backwards => Some((current + count - 1) % count),
            Some(current) => Some((current + 1) % count),
            None => {
                let origin = editor.read(cx).selected_range().start;
                if backwards {
                    match_before(&search.matches, origin)
                } else {
                    match_at_or_after(&search.matches, origin)
                }
            }
        };
        self.apply_file_search_to_editor(true, true, cx);
    }

    fn file_search_writable(&self) -> bool {
        self.file_search
            .as_ref()
            .filter(|search| search.open)
            .and_then(|search| self.right_panel_file_editors.get(&search.path))
            .is_some_and(|editor| editor.writable)
    }

    fn file_search_replace_current(&mut self, cx: &mut Context<Self>) {
        if !self.file_search_writable() {
            return;
        }
        let Some(editor) = self.file_search_editor() else {
            return;
        };
        let Some(search) = self.file_search.as_ref() else {
            return;
        };
        let Some(range) = search
            .current
            .and_then(|index| search.matches.get(index))
            .cloned()
        else {
            return;
        };
        let CompiledSearch::Ready(regex) = compile_search(
            search.query.read(cx).content(),
            search.case_sensitive,
            search.whole_word,
            search.use_regex,
        ) else {
            return;
        };
        let template = search.replace.read(cx).content().to_owned();
        let use_regex = search.use_regex;
        let expanded = expand_replacement(
            &regex,
            editor.read(cx).content(),
            &range,
            &template,
            use_regex,
        );
        editor.update(cx, |editor, cx| {
            editor.replace_range(range, &expanded, cx);
        });
        // Recompute now rather than waiting for the queued edit event, then
        // walk on to the next match after the caret the replacement left.
        self.refresh_file_search(SearchRefresh::Content, cx);
        let caret = editor.read(cx).cursor();
        if let Some(search) = self.file_search.as_mut().filter(|search| search.open)
            && !search.matches.is_empty()
        {
            search.current = match_at_or_after(&search.matches, caret);
            self.apply_file_search_to_editor(true, true, cx);
        }
    }

    fn file_search_replace_all(&mut self, cx: &mut Context<Self>) {
        if !self.file_search_writable() {
            return;
        }
        let Some(editor) = self.file_search_editor() else {
            return;
        };
        let Some(search) = self.file_search.as_ref() else {
            return;
        };
        let CompiledSearch::Ready(regex) = compile_search(
            search.query.read(cx).content(),
            search.case_sensitive,
            search.whole_word,
            search.use_regex,
        ) else {
            return;
        };
        let template = search.replace.read(cx).content().to_owned();
        let use_regex = search.use_regex;
        // Rescan without the cap: a limited highlight list must not make
        // Replace All silently skip the tail of the file.
        let replaced = {
            let content = editor.read(cx).content();
            let (matches, _) = collect_search_matches(content, &regex, None);
            if matches.is_empty() {
                return;
            }
            replace_all_content(content, &regex, &matches, &template, use_regex)
        };
        // Through `replace_range`, not `set_content`: a whole-content splice
        // is a recorded edit, so Replace All is one undo step, while
        // `set_content` would reset the editor's undo history as a reload.
        editor.update(cx, |editor, cx| {
            let len = editor.content().len();
            editor.replace_range(0..len, &replaced, cx);
        });
        self.refresh_file_search(SearchRefresh::Content, cx);
    }

    fn toggle_file_search_flag(&mut self, flag: SearchFlagKind, cx: &mut Context<Self>) {
        let Some(search) = self.file_search.as_mut().filter(|search| search.open) else {
            cx.propagate();
            return;
        };
        match flag {
            SearchFlagKind::CaseSensitive => search.case_sensitive = !search.case_sensitive,
            SearchFlagKind::WholeWord => search.whole_word = !search.whole_word,
            SearchFlagKind::Regex => search.use_regex = !search.use_regex,
        }
        self.refresh_file_search(SearchRefresh::Query, cx);
    }

    // Action handlers, registered on the root in `render`.

    pub(super) fn open_find_action(
        &mut self,
        _: &OpenFind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.file_search_target_focused(window, cx) {
            if self.transcript_search_open() {
                self.close_transcript_search(false, window, cx);
            }
            self.open_file_search(false, window, cx);
        } else {
            if self.file_search_open() {
                self.close_file_search_with_focus(false, window, cx);
            }
            self.open_transcript_search(window, cx);
        }
    }

    pub(super) fn open_find_replace_action(
        &mut self,
        _: &OpenFindReplace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.transcript_search_open() {
            self.close_transcript_search(false, window, cx);
        }
        self.open_file_search(true, window, cx);
    }

    pub(super) fn close_find_action(
        &mut self,
        _: &CloseFind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.transcript_search_open() {
            self.close_transcript_search(true, window, cx);
        } else if self.file_search_open() {
            self.close_file_search(window, cx);
        } else {
            // Unhandled, so the keystroke falls through to the next binding —
            // escape reaches CancelTurn when no find bar is up.
            cx.propagate();
        }
    }

    pub(super) fn find_next_action(
        &mut self,
        _: &FindNext,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.transcript_search_open() {
            self.navigate_transcript_search(false, cx);
        } else if self.file_search_open() {
            self.file_search_navigate(false, cx);
        } else {
            cx.propagate();
        }
    }

    pub(super) fn find_previous_action(
        &mut self,
        _: &FindPrevious,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.transcript_search_open() {
            self.navigate_transcript_search(true, cx);
        } else if self.file_search_open() {
            self.file_search_navigate(true, cx);
        } else {
            cx.propagate();
        }
    }

    pub(super) fn toggle_find_case_action(
        &mut self,
        _: &ToggleFindCaseSensitive,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_file_search_flag(SearchFlagKind::CaseSensitive, cx);
    }

    pub(super) fn toggle_find_whole_word_action(
        &mut self,
        _: &ToggleFindWholeWord,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_file_search_flag(SearchFlagKind::WholeWord, cx);
    }

    pub(super) fn toggle_find_regex_action(
        &mut self,
        _: &ToggleFindRegex,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_file_search_flag(SearchFlagKind::Regex, cx);
    }

    pub(super) fn replace_all_matches_action(
        &mut self,
        _: &ReplaceAllMatches,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.file_search_open() {
            self.file_search_replace_all(cx);
        } else {
            cx.propagate();
        }
    }

    /// The find bar, or `None` while closed. The editor body lays it out as a
    /// normal row above the scroll container — the way Zed places its buffer
    /// search bar — so the content and its line-number gutter are pushed down
    /// by exactly the bar's height and the first lines are never covered.
    pub(super) fn render_file_search_bar(
        &self,
        pane_width: f32,
        writable: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let search = self.file_search.as_ref().filter(|search| search.open)?;
        let theme = Theme::current(cx);
        // Both rows share one input width, sized to the pane the way Zed
        // computes its search input width, so the find and replace fields
        // stay exactly aligned at any panel size.
        let input_width = (pane_width - FIND_BAR_CONTROLS_WIDTH).clamp(96.0, 380.0);
        let query_empty = search.query.read(cx).content().is_empty();
        let has_matches = !search.matches.is_empty();
        let replace_shown = search.replace_visible && writable;

        let count_label: Option<SharedString> = if query_empty {
            None
        } else if search.invalid {
            Some(tr!("find.bad_pattern").into())
        } else if !has_matches {
            Some(tr!("find.no_results").into())
        } else {
            let current = search.current.map(|index| index + 1).unwrap_or(0);
            let total = search.matches.len();
            let suffix = if search.limited { "+" } else { "" };
            Some(SharedString::from(tr!(
                "find.result_count",
                current = current,
                total = format!("{total}{suffix}")
            )))
        };
        let count_is_bad = !query_empty && (search.invalid || !has_matches);

        let find_row = div()
            .flex()
            .items_center()
            .gap(px(4.0))
            .child(
                self.find_input_box(
                    "find-query",
                    &search.query,
                    input_width,
                    search.invalid,
                    window,
                    cx,
                )
                .child(find_toggle(
                    "find-toggle-case",
                    "icons/case-sensitive.svg",
                    tr!(
                        "find.match_case",
                        shortcut = crate::platform::primary_shortcut("⌥⌘C", "Ctrl+Alt+C")
                    ),
                    search.case_sensitive,
                    theme,
                    cx.listener(|this, _, _, cx| {
                        this.toggle_file_search_flag(SearchFlagKind::CaseSensitive, cx);
                    }),
                ))
                .child(find_toggle(
                    "find-toggle-word",
                    "icons/whole-word.svg",
                    tr!(
                        "find.match_whole_word",
                        shortcut = crate::platform::primary_shortcut("⌥⌘W", "Ctrl+Alt+W")
                    ),
                    search.whole_word,
                    theme,
                    cx.listener(|this, _, _, cx| {
                        this.toggle_file_search_flag(SearchFlagKind::WholeWord, cx);
                    }),
                ))
                .child(find_toggle(
                    "find-toggle-regex",
                    "icons/regex.svg",
                    tr!(
                        "find.use_regex",
                        shortcut = crate::platform::primary_shortcut("⌥⌘R", "Ctrl+Alt+R")
                    ),
                    search.use_regex,
                    theme,
                    cx.listener(|this, _, _, cx| {
                        this.toggle_file_search_flag(SearchFlagKind::Regex, cx);
                    }),
                )),
            )
            .child(
                div()
                    .min_w(px(56.0))
                    .flex_none()
                    .text_size(sp(12.5))
                    .whitespace_nowrap()
                    .text_color(if count_is_bad {
                        theme.danger
                    } else {
                        theme.text_tertiary
                    })
                    .children(count_label),
            )
            .child(
                div()
                    .w(px(1.0))
                    .h(px(16.0))
                    .flex_none()
                    .bg(theme.border_strong),
            )
            .child(find_bar_button(
                "find-previous",
                "icons/arrow-up.svg",
                tr!("find.previous_match"),
                has_matches,
                theme,
                cx.listener(|this, _, _, cx| this.file_search_navigate(true, cx)),
            ))
            .child(find_bar_button(
                "find-next",
                "icons/arrow-down.svg",
                tr!("find.next_match"),
                has_matches,
                theme,
                cx.listener(|this, _, _, cx| this.file_search_navigate(false, cx)),
            ))
            .child(div().flex_1())
            .child(find_bar_button(
                "find-close",
                "icons/x.svg",
                tr!("find.close"),
                true,
                theme,
                cx.listener(|this, _, window, cx| this.close_file_search(window, cx)),
            ));

        let replace_row = replace_shown.then(|| {
            div()
                .flex()
                .items_center()
                .gap(px(4.0))
                .child(self.find_input_box(
                    "find-replace",
                    &search.replace,
                    input_width,
                    false,
                    window,
                    cx,
                ))
                .child(find_bar_button(
                    "replace-one",
                    "icons/replace.svg",
                    tr!("find.replace"),
                    has_matches,
                    theme,
                    cx.listener(|this, _, _, cx| this.file_search_replace_current(cx)),
                ))
                .child(find_bar_button(
                    "replace-all",
                    "icons/replace-all.svg",
                    tr!(
                        "find.replace_all",
                        shortcut = crate::platform::primary_shortcut("⌥⌘Enter", "Ctrl+Alt+Enter")
                    ),
                    has_matches,
                    theme,
                    cx.listener(|this, _, _, cx| this.file_search_replace_all(cx)),
                ))
        });

        Some(
            div()
                .id("file-search-bar")
                .key_context("FindBar")
                .w_full()
                .flex_none()
                .overflow_hidden()
                .border_b_1()
                .border_color(theme.border)
                .bg(theme.surface)
                .font_family(".SystemUIFont")
                .cursor_default()
                .flex()
                .items_start()
                .py(px(6.0))
                .pl(px(4.0))
                .pr(px(10.0))
                .child(
                    div()
                        .id("find-toggle-replace")
                        .w(px(18.0))
                        .h(px(24.0))
                        .flex_none()
                        .rounded(px(4.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_default()
                        .when(writable, |element| {
                            element
                                .hover(|element| element.bg(theme.overlay))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    if let Some(search) =
                                        this.file_search.as_mut().filter(|search| search.open)
                                    {
                                        search.replace_visible = !search.replace_visible;
                                        cx.notify();
                                    }
                                }))
                                .tooltip(|window, cx| {
                                    Tooltip::new(tr!("find.toggle_replace")).build(window, cx)
                                })
                        })
                        .child(icon(
                            if replace_shown {
                                "icons/chevron-down.svg"
                            } else {
                                "icons/chevron-right.svg"
                            },
                            10.0,
                            if writable {
                                theme.text_tertiary
                            } else {
                                theme.text_ghost
                            },
                        )),
                )
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .flex()
                        .flex_col()
                        .gap(px(4.0))
                        .pl(px(2.0))
                        .child(find_row)
                        .children(replace_row),
                )
                .into_any_element(),
        )
    }

    /// One input box of the find bar: a bordered inset field that carries the
    /// focus ring, with optional trailing children (the query's toggles).
    fn find_input_box(
        &self,
        id: &'static str,
        input: &Entity<TextInput>,
        width: f32,
        invalid: bool,
        window: &Window,
        cx: &Context<Self>,
    ) -> Stateful<Div> {
        let theme = Theme::current(cx);
        let focused = input.read(cx).is_visually_focused(window);
        div()
            .id(id)
            .w(px(width))
            .flex_none()
            .min_h(px(24.0))
            .px(px(5.0))
            .py(px(2.0))
            .rounded(px(5.0))
            .border_1()
            .border_color(if invalid {
                theme.danger
            } else if focused {
                theme.accent
            } else {
                theme.border_strong
            })
            .bg(theme.inset)
            .flex()
            .items_center()
            .gap(px(2.0))
            .text_size(sp(12.5))
            .line_height(sp(16.0))
            .child(div().min_w_0().flex_1().child(input.clone()))
    }
}

#[derive(Clone, Copy)]
enum SearchFlagKind {
    CaseSensitive,
    WholeWord,
    Regex,
}

/// A small square toggle inside the find input, VS Code style: quiet when
/// off, accent-tinted when on.
fn find_toggle(
    id: &'static str,
    icon_path: &'static str,
    tooltip_label: String,
    active: bool,
    theme: Theme,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    div()
        .id(id)
        .size(px(18.0))
        .flex_none()
        .rounded(px(4.0))
        .flex()
        .items_center()
        .justify_center()
        .cursor_default()
        // The border is always present so toggling never shifts the glyph.
        .border_1()
        .border_color(if active {
            theme.accent.opacity(0.5)
        } else {
            gpui::transparent_black()
        })
        .when(active, |element| element.bg(theme.accent.opacity(0.22)))
        .when(!active, |element| {
            element.hover(|element| element.bg(theme.overlay_strong))
        })
        .child(icon(
            icon_path,
            12.0,
            if active {
                theme.text
            } else {
                theme.text_tertiary
            },
        ))
        .tooltip(move |window, cx| Tooltip::new(tooltip_label.clone()).build(window, cx))
        .on_click(move |event, window, cx| {
            cx.stop_propagation();
            on_click(event, window, cx);
        })
}

/// A ghost icon button in the bar; disabled ones dim and ignore clicks.
fn find_bar_button(
    id: &'static str,
    icon_path: &'static str,
    tooltip_label: String,
    enabled: bool,
    theme: Theme,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    div()
        .id(id)
        .size(px(22.0))
        .flex_none()
        .rounded(px(5.0))
        .flex()
        .items_center()
        .justify_center()
        .cursor_default()
        .child(icon(
            icon_path,
            12.0,
            if enabled {
                theme.text_secondary
            } else {
                theme.text_ghost
            },
        ))
        .tooltip(move |window, cx| Tooltip::new(tooltip_label.clone()).build(window, cx))
        .when(enabled, |element| {
            element
                .hover(|element| element.bg(theme.overlay))
                .active(|element| element.bg(theme.overlay_strong))
                .on_click(move |event, window, cx| {
                    cx.stop_propagation();
                    on_click(event, window, cx);
                })
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matches_of(
        content: &str,
        query: &str,
        case: bool,
        word: bool,
        regexp: bool,
    ) -> Vec<Range<usize>> {
        match compile_search(query, case, word, regexp) {
            CompiledSearch::Ready(regex) => collect_search_matches(content, &regex, None).0,
            _ => Vec::new(),
        }
    }

    #[test]
    fn literal_search_is_case_insensitive_until_toggled() {
        let content = "Foo foo FOO foobar";
        assert_eq!(
            matches_of(content, "foo", false, false, false),
            vec![0..3, 4..7, 8..11, 12..15]
        );
        assert_eq!(
            matches_of(content, "foo", true, false, false),
            vec![4..7, 12..15]
        );
    }

    #[test]
    fn whole_word_respects_boundaries_in_both_modes() {
        let content = "cat catalog concat cat";
        assert_eq!(
            matches_of(content, "cat", false, true, false),
            vec![0..3, 19..22]
        );
        // Regex alternation still gets wrapped as one word.
        assert_eq!(
            matches_of(content, "cat|dog", false, true, true),
            vec![0..3, 19..22]
        );
    }

    #[test]
    fn regex_mode_matches_patterns_and_flags_bad_ones() {
        let content = "id: 12, id: 345";
        assert_eq!(
            matches_of(content, r"\d+", false, false, true),
            vec![4..6, 12..15]
        );
        assert!(matches!(
            compile_search("(unclosed", false, false, true),
            CompiledSearch::Invalid
        ));
        // The same text is a fine literal query.
        assert_eq!(
            matches_of("(unclosed", "(unclosed", false, false, false),
            vec![0..9]
        );
        assert!(matches!(
            compile_search("", false, false, true),
            CompiledSearch::Empty
        ));
    }

    #[test]
    fn zero_width_matches_are_skipped() {
        assert_eq!(matches_of("aab", "a*", false, false, true), vec![0..2]);
    }

    #[test]
    fn match_collection_stops_at_the_cap_and_says_so() {
        let content = "x ".repeat(10);
        let CompiledSearch::Ready(regex) = compile_search("x", false, false, false) else {
            panic!("literal query must compile");
        };
        let (matches, limited) = collect_search_matches(&content, &regex, Some(4));
        assert_eq!(matches.len(), 4);
        assert!(limited);
        let (matches, limited) = collect_search_matches(&content, &regex, None);
        assert_eq!(matches.len(), 10);
        assert!(!limited);
    }

    #[test]
    fn replacement_expands_captures_only_in_regex_mode() {
        let content = "let alpha = 1;";
        let CompiledSearch::Ready(regex) = compile_search(r"(\w+) = (\d+)", false, false, true)
        else {
            panic!("pattern must compile");
        };
        let range = 4..13;
        assert_eq!(
            expand_replacement(&regex, content, &range, "$2 = $1", true),
            "1 = alpha"
        );
        // Literal mode: the template is inserted verbatim, dollars included.
        let CompiledSearch::Ready(literal) = compile_search("alpha", false, false, false) else {
            panic!("literal query must compile");
        };
        assert_eq!(
            expand_replacement(&literal, content, &(4..9), "$1", false),
            "$1"
        );
    }

    #[test]
    fn replace_all_splices_every_listed_match() {
        let content = "one two one";
        let CompiledSearch::Ready(regex) = compile_search("one", false, false, false) else {
            panic!("literal query must compile");
        };
        let (matches, _) = collect_search_matches(content, &regex, None);
        assert_eq!(
            replace_all_content(content, &regex, &matches, "three", false),
            "three two three"
        );
    }

    #[test]
    fn navigation_anchors_wrap_in_both_directions() {
        let matches = vec![2..4, 10..12, 20..22];
        assert_eq!(match_at_or_after(&matches, 0), Some(0));
        assert_eq!(match_at_or_after(&matches, 10), Some(1));
        assert_eq!(match_at_or_after(&matches, 11), Some(2));
        assert_eq!(match_at_or_after(&matches, 23), Some(0));
        assert_eq!(match_before(&matches, 0), Some(2));
        assert_eq!(match_before(&matches, 10), Some(0));
        // An offset inside a match still counts that match as "before" it, so
        // a first shift-enter lands on the occurrence under the caret.
        assert_eq!(match_before(&matches, 11), Some(1));
        assert_eq!(match_before(&matches, 21), Some(2));
        assert_eq!(match_at_or_after(&[], 5), None);
        assert_eq!(match_before(&[], 5), None);
    }

    #[test]
    fn reveal_scrolls_only_when_the_match_is_outside_the_viewport() {
        // Visible with margin: no scroll.
        assert_eq!(
            revealed_scroll_offset(
                px(-100.0),
                px(500.0),
                px(200.0),
                px(16.0),
                px(100.0),
                px(400.0)
            ),
            None
        );
        // Below the viewport: lands ~35% down, offset moves up (more negative).
        let next = revealed_scroll_offset(
            px(-100.0),
            px(500.0),
            px(600.0),
            px(16.0),
            px(100.0),
            px(400.0),
        )
        .expect("must scroll");
        assert_eq!(next, px(-100.0) + (px(205.0) - px(600.0)));
        // Clamped to the scrollable range at both ends.
        assert_eq!(
            revealed_scroll_offset(
                px(-100.0),
                px(120.0),
                px(600.0),
                px(16.0),
                px(100.0),
                px(400.0)
            ),
            Some(px(-120.0))
        );
        assert_eq!(
            revealed_scroll_offset(
                px(-100.0),
                px(500.0),
                px(50.0),
                px(16.0),
                px(100.0),
                px(400.0)
            ),
            Some(px(0.0))
        );
    }
}

//! The composer's autocompletion popup: slash commands and `@` file mentions.
//!
//! The popup is a pure view over prefetched data. Command and file indexes are
//! discovered on the background executor into `QueryCache`s and mirrored into
//! plain fields the frame reads; the filter over them is memoized per
//! keystroke, so a caret blink re-renders the popup without re-fuzzy-matching
//! the workspace.
//!
//! Keys follow the model picker's split: the composer keeps real focus the
//! whole time and the popup's selection is only drawn. While the popup is
//! open the composer card declares the `ComposerAutocomplete` key context, and
//! `up`/`down`/`enter`/`tab`/`escape` reach it as actions that outrank the
//! field's own bindings; when it closes the context disappears and `enter`
//! submits again.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gpui::{
    Anchor, App, Bounds, Font, KeyBinding, Pixels, StyledText, TextRun, anchored, deferred,
};
use nucleo_matcher::Matcher;

use crate::composer_complete::{
    self, FILE_INDEX_CAP, FileEntry, Scored, SlashCommand, Trigger, TriggerKind,
    highlight_byte_ranges,
};
use crate::ui::menu::{ConfirmEntry, DismissMenu, SelectNextEntry, SelectPreviousEntry};

use super::composer::next_picker_highlight;
use super::*;

/// Key context the composer card declares while the popup is open.
const AUTOCOMPLETE_CONTEXT: &str = "ComposerAutocomplete > ComposerInput";

/// Bind the popup's keys. Must run after [`crate::input::init`]: `enter` and
/// the arrows tie with the field's own bindings at the `ComposerInput` depth,
/// and the tie goes to whichever was registered last.
pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("down", SelectNextEntry, Some(AUTOCOMPLETE_CONTEXT)),
        KeyBinding::new("up", SelectPreviousEntry, Some(AUTOCOMPLETE_CONTEXT)),
        KeyBinding::new("enter", ConfirmEntry, Some(AUTOCOMPLETE_CONTEXT)),
        KeyBinding::new("tab", ConfirmEntry, Some(AUTOCOMPLETE_CONTEXT)),
        KeyBinding::new("escape", DismissMenu, Some(AUTOCOMPLETE_CONTEXT)),
    ]);
}

pub(super) enum AutocompleteRow {
    Command(Scored<SlashCommand>),
    File(Scored<FileEntry>),
}

/// Filter results for one (kind, query, source index) — the popup's rows are
/// recomputed on a keystroke, not on every frame the caret blinks.
struct ResultsMemo {
    kind: TriggerKind,
    query: String,
    /// `Rc::as_ptr` identity of the source index the rows were filtered from.
    source: usize,
    rows: Rc<Vec<AutocompleteRow>>,
}

/// Cross-frame state for the popup. All interior-mutable: the render path
/// reconciles it from `&self`, the same way the transcript anchors do.
pub(super) struct AutocompleteUi {
    /// The trigger as of the last frame, for detecting query/site changes.
    token: RefCell<Option<Trigger>>,
    /// Keyboard cursor over the filtered rows: the popup opens with the first
    /// row selected, and every token change snaps back to it so the best
    /// match is always the one `enter` takes. Clamped to the list at use.
    highlight: Cell<usize>,
    /// Escape pressed on the current token; cleared the moment it changes.
    dismissed: Cell<bool>,
    scroll: ScrollHandle,
    /// The composer card's bounds as of the last frame, recorded by a probe,
    /// so the popup can anchor above the card at the card's own width.
    card_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    results: RefCell<Option<ResultsMemo>>,
    matcher: RefCell<Matcher>,
}

impl AutocompleteUi {
    pub(super) fn new() -> Self {
        Self {
            token: RefCell::new(None),
            highlight: Cell::new(0),
            dismissed: Cell::new(false),
            scroll: ScrollHandle::new(),
            card_bounds: Rc::new(Cell::new(None)),
            results: RefCell::new(None),
            matcher: RefCell::new(composer_complete::matcher()),
        }
    }

    /// The cell the composer card's bounds probe writes into.
    pub(super) fn card_bounds_cell(&self) -> Rc<Cell<Option<Bounds<Pixels>>>> {
        self.card_bounds.clone()
    }
}

impl Waku {
    /// Refresh the drawn command and file indexes for the selected session.
    ///
    /// A cache hit lands immediately; a miss starts discovery on the
    /// background executor and re-runs this when it arrives. Nothing here may
    /// touch the filesystem directly.
    pub(super) fn refresh_composer_sources(&mut self, cx: &mut Context<Self>) {
        let Some(project_path) = self
            .selected_workspace_path()
            .map(std::path::Path::to_path_buf)
        else {
            self.slash_command_index = Rc::new(Vec::new());
            self.slash_command_index_key = None;
            self.mention_file_index = Rc::new(Vec::new());
            self.mention_file_index_path = None;
            return;
        };
        let provider = self
            .selected_session()
            .map(|session| session.provider)
            .unwrap_or(self.state.last_provider);
        let reported = self
            .selected_session()
            .map(|session| session.available_commands.clone())
            .unwrap_or_default();

        let command_key = (provider, project_path.clone());
        match self.slash_commands.read(&command_key) {
            Query::Ready(commands) => {
                self.slash_command_index = Rc::new(composer_complete::merge_reported_commands(
                    &commands, &reported,
                ));
                self.slash_command_index_key = Some(command_key);
            }
            Query::Pending => {
                // A scan for this exact key is in flight; anything drawn
                // meanwhile must not be another provider's list.
                if self.slash_command_index_key.as_ref() != Some(&command_key) {
                    self.slash_command_index = Rc::new(Vec::new());
                    self.slash_command_index_key = None;
                }
            }
            Query::Missing(token) => {
                if self.slash_command_index_key.as_ref() != Some(&command_key) {
                    self.slash_command_index = Rc::new(Vec::new());
                    self.slash_command_index_key = None;
                }
                let path = project_path.clone();
                let workspace = waku_client::WorkspaceClient::new(self.daemon.client());
                cx.spawn(async move |waku, cx| {
                    let commands = cx
                        .background_executor()
                        .spawn(async move {
                            match workspace.request(
                                waku_client::WorkspaceOperation::DiscoverSlashCommands {
                                    provider,
                                    project_root: path,
                                },
                            ) {
                                Ok(waku_client::WorkspaceResult::SlashCommands { commands }) => {
                                    commands
                                }
                                Ok(_) | Err(_) => Vec::new(),
                            }
                        })
                        .await;
                    waku.update(cx, |waku, cx| {
                        if waku.slash_commands.fulfill(token, commands) {
                            waku.refresh_composer_sources(cx);
                            cx.notify();
                        }
                    })
                    .ok();
                })
                .detach();
            }
        }

        match self.mention_files.read(&project_path) {
            Query::Ready(files) => {
                self.mention_file_index = files.as_ref().clone().into();
                self.mention_file_index_path = Some(project_path);
            }
            Query::Pending => {
                if self.mention_file_index_path.as_ref() != Some(&project_path) {
                    self.mention_file_index = Rc::new(Vec::new());
                    self.mention_file_index_path = None;
                }
            }
            Query::Missing(token) => {
                if self.mention_file_index_path.as_ref() != Some(&project_path) {
                    self.mention_file_index = Rc::new(Vec::new());
                    self.mention_file_index_path = None;
                }
                let path = project_path.clone();
                let workspace = waku_client::WorkspaceClient::new(self.daemon.client());
                cx.spawn(async move |waku, cx| {
                    let files = cx
                        .background_executor()
                        .spawn(async move {
                            match workspace.request(
                                waku_client::WorkspaceOperation::ListProjectFiles {
                                    root: path,
                                    cap: FILE_INDEX_CAP,
                                },
                            ) {
                                Ok(waku_client::WorkspaceResult::ProjectFiles { entries }) => {
                                    entries
                                }
                                Ok(_) | Err(_) => Vec::new(),
                            }
                        })
                        .await;
                    waku.update(cx, |waku, cx| {
                        if waku.mention_files.fulfill(token, files) {
                            waku.refresh_composer_sources(cx);
                            cx.notify();
                        }
                    })
                    .ok();
                })
                .detach();
            }
        }
    }

    /// Invalidate and re-request both indexes for the selected workspace.
    pub(super) fn invalidate_composer_sources(&mut self, cx: &mut Context<Self>) {
        if let Some(path) = self
            .selected_workspace_path()
            .map(std::path::Path::to_path_buf)
        {
            let provider = self
                .selected_session()
                .map(|session| session.provider)
                .unwrap_or(self.state.last_provider);
            self.slash_commands.invalidate(&(provider, path.clone()));
            self.mention_files.invalidate(&path);
        }
        self.refresh_composer_sources(cx);
    }

    /// The trigger under the composer's caret, reconciled with the popup's
    /// cross-frame state. `None` while the composer is unfocused, the token is
    /// dismissed, or there is nothing to complete.
    fn composer_trigger(&self, window: &Window, cx: &App) -> Option<Trigger> {
        let input = self.composer.read(cx);
        let trigger = if input.focus().is_focused(window) {
            composer_complete::detect_trigger(input.content(), input.cursor())
        } else {
            None
        };
        let ui = &self.composer_autocomplete;
        if *ui.token.borrow() != trigger {
            *ui.token.borrow_mut() = trigger.clone();
            // A different token renumbers the rows: the keyboard cursor and a
            // standing dismissal both describe the previous list.
            ui.highlight.set(0);
            ui.dismissed.set(false);
            ui.scroll.scroll_to_item(0);
        }
        if ui.dismissed.get() {
            return None;
        }
        trigger
    }

    /// The filtered rows for `trigger`, shared by the popup body, the keyboard
    /// cursor and `enter` so an index always means the same row everywhere.
    fn autocomplete_rows(&self, trigger: &Trigger) -> Rc<Vec<AutocompleteRow>> {
        let source = match trigger.kind {
            TriggerKind::Command => Rc::as_ptr(&self.slash_command_index) as usize,
            TriggerKind::File => Rc::as_ptr(&self.mention_file_index) as usize,
        };
        {
            let memo = self.composer_autocomplete.results.borrow();
            if let Some(memo) = memo.as_ref().filter(|memo| {
                memo.kind == trigger.kind && memo.query == trigger.query && memo.source == source
            }) {
                return memo.rows.clone();
            }
        }
        let mut matcher = self.composer_autocomplete.matcher.borrow_mut();
        let rows = match trigger.kind {
            TriggerKind::Command => composer_complete::filter_commands(
                &self.slash_command_index,
                &trigger.query,
                &mut matcher,
            )
            .into_iter()
            .map(AutocompleteRow::Command)
            .collect::<Vec<_>>(),
            TriggerKind::File => composer_complete::filter_files(
                &self.mention_file_index,
                &trigger.query,
                &mut matcher,
            )
            .into_iter()
            .map(AutocompleteRow::File)
            .collect(),
        };
        let rows = Rc::new(rows);
        *self.composer_autocomplete.results.borrow_mut() = Some(ResultsMemo {
            kind: trigger.kind,
            query: trigger.query.clone(),
            source,
            rows: rows.clone(),
        });
        rows
    }

    pub(super) fn move_autocomplete_highlight(
        &mut self,
        key: &str,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let Some(trigger) = self.composer_trigger(window, cx) else {
            return;
        };
        let rows = self.autocomplete_rows(&trigger);
        let ui = &self.composer_autocomplete;
        let current = ui.highlight.get().min(rows.len().saturating_sub(1));
        let Some(next) = next_picker_highlight(Some(current), rows.len(), key) else {
            return;
        };
        ui.highlight.set(next);
        ui.scroll.scroll_to_item(next);
        cx.notify();
    }

    /// Insert the chosen row over the trigger token. `index` comes from a
    /// click; `None` is the keyboard path, which takes the drawn cursor and
    /// defaults to the first row so `enter` works the moment the popup opens.
    pub(super) fn accept_autocomplete(
        &mut self,
        index: Option<usize>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let Some(trigger) = self.composer_trigger(window, cx) else {
            return;
        };
        let rows = self.autocomplete_rows(&trigger);
        let index = index.unwrap_or_else(|| {
            self.composer_autocomplete
                .highlight
                .get()
                .min(rows.len().saturating_sub(1))
        });
        let Some(row) = rows.get(index) else {
            return;
        };
        let insert = match row {
            AutocompleteRow::Command(scored) => format!("/{} ", scored.item.name),
            AutocompleteRow::File(scored) => format!("@{} ", scored.item.path),
        };
        self.composer.update(cx, |input, cx| {
            input.replace_range(trigger.range.clone(), &insert, cx);
        });
        cx.notify();
    }

    pub(super) fn dismiss_autocomplete(&mut self, cx: &mut Context<Self>) {
        self.composer_autocomplete.dismissed.set(true);
        cx.notify();
    }

    /// The popup, anchored above the composer card, or `None` when idle.
    ///
    /// Reads only the prefetched indexes — discovery never runs on a frame.
    pub(super) fn render_composer_autocomplete(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let trigger = self.composer_trigger(window, cx)?;
        let rows = self.autocomplete_rows(&trigger);
        if rows.is_empty() {
            return None;
        }
        // The probe records during paint, so the first frame a composer ever
        // draws has no bounds yet; the popup appears one frame later.
        let card_bounds = self.composer_autocomplete.card_bounds.get()?;
        let theme = Theme::current(cx);
        let highlight = self
            .composer_autocomplete
            .highlight
            .get()
            .min(rows.len().saturating_sub(1));

        let mut list = div()
            .id("composer-autocomplete-list")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.composer_autocomplete.scroll)
            .p(px(4.0));
        for (index, row) in rows.iter().enumerate() {
            list =
                list.child(self.render_autocomplete_row(index, row, highlight, &theme, window, cx));
        }

        Some(
            deferred(
                anchored()
                    .position(point(card_bounds.origin.x, card_bounds.origin.y - px(6.0)))
                    .anchor(Anchor::BottomLeft)
                    .snap_to_window_with_margin(px(8.0))
                    .child(
                        div()
                            .occlude()
                            .w(card_bounds.size.width)
                            .max_h(px(302.0))
                            .rounded(px(11.0))
                            .border_1()
                            .border_color(theme.border_strong)
                            .bg(theme.raised)
                            .shadow_lg()
                            .flex()
                            .flex_col()
                            .overflow_hidden()
                            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                                this.dismiss_autocomplete(cx);
                            }))
                            .child(list),
                    ),
            )
            .with_priority(1)
            .into_any_element(),
        )
    }

    fn render_autocomplete_row(
        &self,
        index: usize,
        row: &AutocompleteRow,
        highlight: usize,
        theme: &Theme,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let highlighted = highlight == index;
        let font = window.text_style().font();
        let base = div()
            .id(index)
            .h(px(30.0))
            .px(px(8.0))
            .rounded(px(6.0))
            .flex()
            .items_center()
            .gap(px(8.0))
            .cursor_default()
            .when(highlighted, |element| element.bg(theme.overlay_strong))
            .hover(|element| element.bg(theme.overlay))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    this.accept_autocomplete(Some(index), window, cx);
                }),
            );
        match row {
            AutocompleteRow::Command(scored) => {
                let command = &scored.item;
                let icon_path = if command.scope == composer_complete::CommandScope::Skill {
                    "icons/sparkle.svg"
                } else {
                    "icons/slash.svg"
                };
                // Positions index the bare name; the drawn `/` shifts every
                // byte range right by one.
                let name_ranges = highlight_byte_ranges(&command.name, &scored.positions, 0)
                    .into_iter()
                    .map(|range| range.start + 1..range.end + 1)
                    .collect();
                let mut name_font = font.clone();
                name_font.weight = FontWeight::MEDIUM;
                base.child(icon(icon_path, 12.0, theme.text_tertiary))
                    .child(
                        div()
                            .flex_none()
                            .max_w(px(260.0))
                            .truncate()
                            .text_size(px(12.0))
                            .child(matched_text(
                                format!("/{}", command.name),
                                name_ranges,
                                theme.text,
                                theme.accent,
                                name_font,
                            )),
                    )
                    .when_some(command.argument_hint.clone(), |element, hint| {
                        element.child(
                            div()
                                .flex_none()
                                .text_size(px(11.0))
                                .text_color(theme.text_ghost)
                                .child(hint),
                        )
                    })
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(11.0))
                            .text_color(theme.text_tertiary)
                            .child(SharedString::from(command.description.clone())),
                    )
                    .child(
                        div()
                            .h(px(16.0))
                            .px(px(5.0))
                            .flex_none()
                            .rounded(px(4.0))
                            .border_1()
                            .border_color(theme.border)
                            .flex()
                            .items_center()
                            .text_size(px(9.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.text_tertiary)
                            .child(command.scope.label()),
                    )
                    .into_any_element()
            }
            AutocompleteRow::File(scored) => {
                let file = &scored.item;
                // The row draws basename then directory, but the positions
                // index the full path — each segment recovers its own ranges.
                // A directory's trailing slash stays with the basename, so a
                // match on it still paints.
                let trimmed_len = file.path.trim_end_matches('/').len();
                let name_start = file.path[..trimmed_len]
                    .rfind('/')
                    .map_or(0, |index| index + 1);
                let name = &file.path[name_start..];
                let parent = &file.path[..name_start.saturating_sub(1)];
                let name_char_offset = file.path[..name_start].chars().count();
                let icon_path = if file.is_dir {
                    "icons/folder.svg"
                } else {
                    super::right_panel::file_icon_for_path(&file.path)
                };
                base.child(icon(icon_path, 13.0, theme.text_tertiary))
                    .child(
                        div()
                            .flex_none()
                            .max_w(px(300.0))
                            .truncate()
                            .text_size(px(12.0))
                            .child(matched_text(
                                name.to_owned(),
                                highlight_byte_ranges(name, &scored.positions, name_char_offset),
                                theme.text,
                                theme.accent,
                                font.clone(),
                            )),
                    )
                    .when(!parent.is_empty(), |element| {
                        element.child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_size(px(11.0))
                                .child(matched_text(
                                    parent.to_owned(),
                                    highlight_byte_ranges(parent, &scored.positions, 0),
                                    theme.text_ghost,
                                    theme.accent,
                                    font,
                                )),
                        )
                    })
                    .into_any_element()
            }
        }
    }
}

/// Text with the fuzzy-matched byte ranges lifted to the accent colour and a
/// semibold weight, the runs tiling the string exactly. `ranges` are sorted
/// and non-overlapping, as [`highlight_byte_ranges`] returns them.
fn matched_text(
    text: String,
    ranges: Vec<std::ops::Range<usize>>,
    base_color: Hsla,
    accent: Hsla,
    font: Font,
) -> StyledText {
    // A step above either base weight in these rows — regular file paths and
    // medium command names both read as "a bit bolder", not shouting.
    let mut accent_font = font.clone();
    accent_font.weight = FontWeight::SEMIBOLD;
    let run = |len: usize, font: Font, color: Hsla| TextRun {
        len,
        font,
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let mut runs = Vec::new();
    let mut cursor = 0;
    for range in ranges {
        if range.start > cursor {
            runs.push(run(range.start - cursor, font.clone(), base_color));
        }
        runs.push(run(range.len(), accent_font.clone(), accent));
        cursor = range.end;
    }
    if cursor < text.len() {
        runs.push(run(text.len() - cursor, font.clone(), base_color));
    }
    StyledText::new(text).with_runs(runs)
}

/// The probe recording the composer card's bounds for the popup's anchor.
/// `inset_0` so it reports the border box, not the padded content box.
pub(super) fn composer_card_bounds_probe(
    cell: Rc<Cell<Option<Bounds<Pixels>>>>,
) -> impl IntoElement {
    canvas(
        move |bounds: Bounds<Pixels>, _, _| cell.set(Some(bounds)),
        |_, _, _, _| (),
    )
    .absolute()
    .inset_0()
}

#[cfg(test)]
mod tests {
    /// The popup renders on every keystroke frame; discovery walks the
    /// filesystem and forks subprocesses. The two must never meet: everything
    /// the render path shows comes from the prefetched indexes.
    #[test]
    fn the_autocomplete_render_path_does_no_filesystem_work() {
        let source = include_str!("./autocomplete.rs");
        let start = source
            .find("\n    fn composer_trigger(")
            .expect("composer_trigger must exist");
        let end = source
            .find("\n/// The probe recording")
            .expect("probe marker must exist");
        let render_paths = &source[start..end];
        for forbidden in [
            "discover_slash_commands(",
            "list_project_files(",
            "std::fs",
            "Command::new",
            "read_dir",
        ] {
            assert!(
                !render_paths.contains(forbidden),
                "the render path must not call `{forbidden}`; \
                 discovery belongs in refresh_composer_sources"
            );
        }
    }
}

use std::ops::Range;
use std::rc::Rc;
use std::time::{Duration, Instant};

use crate::md::highlight::{self, Lang, TokenClass};
use crate::ui::menu::{ContextMenuHandle, MenuItem, context_menu};
use crate::ui::scrollbar::{self, ScrollbarState};
use gpui::{
    App, Bounds, ClipboardEntry, ClipboardItem, Context, CursorStyle, DispatchPhase, Element,
    ElementId, ElementInputHandler, Entity, EntityInputHandler, EventEmitter, FocusHandle,
    Focusable, GlobalElementId, Hsla, InspectorElementId, IntoElement, KeyBinding, LayoutId,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad, Pixels, Point,
    ScrollHandle, SharedString, StyledText, Subscription, Task, TextLayout, TextRun,
    UTF16Selection, UnderlineStyle, Window, actions, div, fill, point, prelude::*, px, size,
};
use unicode_segmentation::UnicodeSegmentation;

use crate::theme::{Theme, sp};

actions!(
    composer,
    [
        Backspace,
        Delete,
        Left,
        Right,
        Up,
        Down,
        SelectLeft,
        SelectRight,
        SelectAll,
        Home,
        End,
        MoveToPreviousWord,
        MoveToNextWord,
        SelectToStart,
        SelectToEnd,
        SelectToPreviousWord,
        SelectToNextWord,
        DeleteToStart,
        DeleteToEnd,
        DeleteToPreviousWord,
        DeleteToNextWord,
        Paste,
        Cut,
        Copy,
        Undo,
        Redo,
        Enter,
        Newline,
        SubmitSteer,
        Clear,
    ]
);

const CURSOR_BLINK_INTERVAL: Duration = Duration::from_millis(500);
const CURSOR_BLINK_PAUSE: Duration = Duration::from_millis(300);

pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("backspace", Backspace, Some("TextInput")),
        KeyBinding::new("delete", Delete, Some("TextInput")),
        KeyBinding::new("alt-backspace", DeleteToPreviousWord, Some("TextInput")),
        KeyBinding::new("alt-delete", DeleteToNextWord, Some("TextInput")),
        KeyBinding::new("left", Left, Some("TextInput")),
        KeyBinding::new("right", Right, Some("TextInput")),
        KeyBinding::new("up", Up, Some("TextInput")),
        KeyBinding::new("down", Down, Some("TextInput")),
        KeyBinding::new("shift-left", SelectLeft, Some("TextInput")),
        KeyBinding::new("shift-right", SelectRight, Some("TextInput")),
        KeyBinding::new("home", Home, Some("TextInput")),
        KeyBinding::new("end", End, Some("TextInput")),
        KeyBinding::new("shift-home", SelectToStart, Some("TextInput")),
        KeyBinding::new("shift-end", SelectToEnd, Some("TextInput")),
        KeyBinding::new("alt-left", MoveToPreviousWord, Some("TextInput")),
        KeyBinding::new("alt-right", MoveToNextWord, Some("TextInput")),
        KeyBinding::new(
            "alt-shift-left",
            SelectToPreviousWord,
            Some("TextInput"),
        ),
        KeyBinding::new("alt-shift-right", SelectToNextWord, Some("TextInput")),
        KeyBinding::new("secondary-a", SelectAll, Some("TextInput")),
        KeyBinding::new("secondary-v", Paste, Some("TextInput")),
        KeyBinding::new("secondary-c", Copy, Some("TextInput")),
        KeyBinding::new("secondary-x", Cut, Some("TextInput")),
        KeyBinding::new("secondary-z", Undo, Some("TextInput")),
        KeyBinding::new("secondary-shift-z", Redo, Some("TextInput")),
        KeyBinding::new("enter", Enter, Some("TextInput")),
        KeyBinding::new("shift-enter", Newline, Some("TextInput")),
        // While a turn is running, Enter queues a follow-up; the platform's
        // primary modifier + Enter injects it when the provider supports it.
        KeyBinding::new("secondary-enter", SubmitSteer, Some("TextInput")),
        // Two-stage escape for fields that opt in via `clear_on_escape`:
        // the handler propagates when the field is empty (or not opted in),
        // and the keystroke falls through to the surface's own escape —
        // dismiss a popover, close the palette, cancel the turn.
        KeyBinding::new("escape", Clear, Some("TextInput")),
    ]);

    #[cfg(target_os = "macos")]
    cx.bind_keys([
        KeyBinding::new("cmd-backspace", DeleteToStart, Some("TextInput")),
        KeyBinding::new("cmd-delete", DeleteToEnd, Some("TextInput")),
        KeyBinding::new("ctrl-h", Backspace, Some("TextInput")),
        KeyBinding::new("ctrl-d", Delete, Some("TextInput")),
        KeyBinding::new("ctrl-u", DeleteToStart, Some("TextInput")),
        KeyBinding::new("ctrl-k", DeleteToEnd, Some("TextInput")),
        KeyBinding::new("ctrl-b", Left, Some("TextInput")),
        KeyBinding::new("ctrl-f", Right, Some("TextInput")),
        KeyBinding::new("cmd-left", Home, Some("TextInput")),
        KeyBinding::new("cmd-right", End, Some("TextInput")),
        KeyBinding::new("cmd-up", Home, Some("TextInput")),
        KeyBinding::new("cmd-down", End, Some("TextInput")),
        KeyBinding::new("ctrl-a", Home, Some("TextInput")),
        KeyBinding::new("ctrl-e", End, Some("TextInput")),
        KeyBinding::new("shift-cmd-left", SelectToStart, Some("TextInput")),
        KeyBinding::new("shift-cmd-right", SelectToEnd, Some("TextInput")),
        KeyBinding::new("cmd-shift-up", SelectToStart, Some("TextInput")),
        KeyBinding::new("cmd-shift-down", SelectToEnd, Some("TextInput")),
        KeyBinding::new("ctrl-shift-a", SelectToStart, Some("TextInput")),
        KeyBinding::new("ctrl-shift-e", SelectToEnd, Some("TextInput")),
    ]);

    // The word-motion chords Windows and the Linux desktops share.
    #[cfg(not(target_os = "macos"))]
    cx.bind_keys([
        KeyBinding::new(
            "ctrl-backspace",
            DeleteToPreviousWord,
            Some("TextInput"),
        ),
        KeyBinding::new("ctrl-delete", DeleteToNextWord, Some("TextInput")),
        KeyBinding::new("ctrl-left", MoveToPreviousWord, Some("TextInput")),
        KeyBinding::new("ctrl-right", MoveToNextWord, Some("TextInput")),
        KeyBinding::new(
            "ctrl-shift-left",
            SelectToPreviousWord,
            Some("TextInput"),
        ),
        KeyBinding::new("ctrl-shift-right", SelectToNextWord, Some("TextInput")),
    ]);
}

struct BlinkCursor {
    visible: bool,
    paused: bool,
    epoch: usize,
    _task: Task<()>,
}

impl BlinkCursor {
    fn new() -> Self {
        Self {
            visible: false,
            paused: false,
            epoch: 0,
            _task: Task::ready(()),
        }
    }

    fn start(&mut self, cx: &mut Context<Self>) {
        self.blink(self.epoch, cx);
    }

    fn stop(&mut self, cx: &mut Context<Self>) {
        self.epoch = 0;
        cx.notify();
    }

    fn visible(&self) -> bool {
        self.paused || self.visible
    }

    fn pause(&mut self, cx: &mut Context<Self>) {
        self.paused = true;
        self.visible = true;
        cx.notify();

        let epoch = self.next_epoch();
        self._task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(CURSOR_BLINK_PAUSE).await;
            if let Some(this) = this.upgrade() {
                this.update(cx, |this, cx| {
                    this.paused = false;
                    this.blink(epoch, cx);
                });
            }
        });
    }

    fn next_epoch(&mut self) -> usize {
        self.epoch += 1;
        self.epoch
    }

    fn blink(&mut self, epoch: usize, cx: &mut Context<Self>) {
        if self.paused || epoch != self.epoch {
            self.visible = true;
            return;
        }

        self.visible = !self.visible;
        cx.notify();

        let epoch = self.next_epoch();
        self._task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(CURSOR_BLINK_INTERVAL).await;
            if let Some(this) = this.upgrade() {
                this.update(cx, |this, cx| this.blink(epoch, cx));
            }
        });
    }
}

/// How long after the previous edit a new one may still coalesce into the
/// same undo step — Zed's transaction group interval.
const UNDO_GROUP_INTERVAL: Duration = Duration::from_millis(300);

/// Undo steps kept before the oldest fall off. Steps are whole coalesced
/// gestures, so this is far more editing than anyone steps back through.
const UNDO_HISTORY_CAP: usize = 1000;

/// One undo step: the text at `start` was `old` and is now `new`. Undoing
/// splices `old` back over `new`; redoing reverses that. A step grows in
/// place while a run of typing or deleting coalesces into it, so the stack
/// stays proportional to gestures, not keystrokes.
struct EditRecord {
    start: usize,
    old: String,
    new: String,
    /// Selection to restore when the step is undone.
    selection_before: Range<usize>,
    selection_reversed_before: bool,
    /// When the newest coalesced edit landed, bounding the group interval.
    edited_at: Instant,
    /// A sealed step never coalesces with later edits. Set at gesture
    /// boundaries: cut, paste, a completion insert, a finished composition.
    sealed: bool,
    /// An IME composition still underway, amended in place as the marked
    /// text changes so the whole entry undoes as one step.
    composing: bool,
}

#[derive(Default)]
struct EditHistory {
    undo: Vec<EditRecord>,
    redo: Vec<EditRecord>,
}

impl EditHistory {
    /// Record a splice of `new_text` over `range`, called with the content
    /// the splice has not yet been applied to. Coalesces with the newest
    /// step where the pair reads as one gesture: an insertion continuing at
    /// the end of an insertion, or a deletion extending a deletion run in
    /// either direction.
    fn record(
        &mut self,
        content: &str,
        range: &Range<usize>,
        new_text: &str,
        selection: Range<usize>,
        selection_reversed: bool,
        now: Instant,
    ) {
        // A whole-content replace (Replace All) arrives as one huge splice;
        // trimming the shared affixes stores only the span that changed.
        let (start, old, new) = trimmed_splice(content, range, new_text);
        if old.is_empty() && new.is_empty() {
            return;
        }
        self.redo.clear();
        if let Some(last) = self.undo.last_mut()
            && !last.sealed
            && !last.composing
            && now.duration_since(last.edited_at) < UNDO_GROUP_INTERVAL
        {
            // Typing run: this insertion continues where the last edit's text
            // ended, which also lets typing extend a replaced selection.
            if old.is_empty() && !last.new.is_empty() && start == last.start + last.new.len() {
                last.new.push_str(&new);
                last.edited_at = now;
                return;
            }
            if new.is_empty() && last.new.is_empty() && !last.old.is_empty() {
                // Backspace run: this deletion ends where the last one started.
                if start + old.len() == last.start {
                    last.start = start;
                    last.old.insert_str(0, &old);
                    last.edited_at = now;
                    return;
                }
                // Forward-delete run: this deletion starts where the last did.
                if start == last.start {
                    last.old.push_str(&old);
                    last.edited_at = now;
                    return;
                }
            }
        }
        self.push(EditRecord {
            start,
            old,
            new,
            selection_before: selection,
            selection_reversed_before: selection_reversed,
            edited_at: now,
            sealed: false,
            composing: false,
        });
    }

    /// Record an IME splice. The whole composition — every marked-text
    /// revision and the final commit — stays one step, amended in place.
    fn record_composition(
        &mut self,
        content: &str,
        range: &Range<usize>,
        new_text: &str,
        selection: Range<usize>,
        selection_reversed: bool,
        now: Instant,
    ) {
        self.redo.clear();
        if let Some(last) = self.undo.last_mut()
            && last.composing
        {
            let span = last.start..last.start + last.new.len();
            if range.start >= span.start && range.end <= span.end {
                last.new
                    .replace_range(range.start - last.start..range.end - last.start, new_text);
                last.edited_at = now;
                return;
            }
            // A splice outside the open composition should not happen; close
            // the step rather than corrupt it.
            last.composing = false;
            last.sealed = true;
        }
        self.push(EditRecord {
            start: range.start,
            old: content[range.clone()].to_owned(),
            new: new_text.to_owned(),
            selection_before: selection,
            selection_reversed_before: selection_reversed,
            edited_at: now,
            sealed: false,
            composing: true,
        });
    }

    /// Close the open composition step, if any. A canceled composition that
    /// nets no change records nothing.
    fn finalize_composition(&mut self) {
        if let Some(last) = self.undo.last_mut()
            && last.composing
        {
            last.composing = false;
            last.sealed = true;
            if last.old == last.new {
                self.undo.pop();
            }
        }
    }

    /// Stop later edits from coalescing into the newest step — a gesture
    /// boundary such as cut, paste, or a completion insert.
    fn seal(&mut self) {
        if let Some(last) = self.undo.last_mut() {
            last.sealed = true;
        }
    }

    fn push(&mut self, record: EditRecord) {
        self.undo.push(record);
        if self.undo.len() > UNDO_HISTORY_CAP {
            let excess = self.undo.len() - UNDO_HISTORY_CAP;
            self.undo.drain(..excess);
        }
    }

    /// Apply the newest undo step to `content`, returning the restored
    /// content and the selection to show. The step must still verifiably
    /// describe `content`; on any mismatch the history is corrupt and is
    /// dropped whole rather than applied wrong.
    fn undo(&mut self, content: &str) -> Option<(String, Range<usize>, bool)> {
        let record = self.undo.pop()?;
        let span = record.start..record.start + record.new.len();
        if content.get(span.clone()) != Some(record.new.as_str()) {
            self.undo.clear();
            self.redo.clear();
            return None;
        }
        let restored = [&content[..span.start], &record.old, &content[span.end..]].concat();
        let selection = record.selection_before.start.min(restored.len())
            ..record.selection_before.end.min(restored.len());
        let selection_reversed = record.selection_reversed_before;
        self.redo.push(record);
        Some((restored, selection, selection_reversed))
    }

    /// Reapply the newest undone step, with the caret after the re-applied
    /// text — where the original edit left it.
    fn redo(&mut self, content: &str) -> Option<(String, Range<usize>, bool)> {
        let record = self.redo.pop()?;
        let span = record.start..record.start + record.old.len();
        if content.get(span.clone()) != Some(record.old.as_str()) {
            self.undo.clear();
            self.redo.clear();
            return None;
        }
        let applied = [&content[..span.start], &record.new, &content[span.end..]].concat();
        let caret = record.start + record.new.len();
        self.undo.push(record);
        Some((applied, caret..caret, false))
    }
}

/// The splice with its common affixes removed: where `range` in `content` is
/// replaced by `new_text`, the returned offset and (old, new) pair cover only
/// the bytes that actually differ. Keeps a whole-content replace from storing
/// two copies of the file when only a few spans changed.
fn trimmed_splice(content: &str, range: &Range<usize>, new_text: &str) -> (usize, String, String) {
    let old = &content[range.clone()];
    let prefix = common_prefix_len(old, new_text);
    let (old, new_text) = (&old[prefix..], &new_text[prefix..]);
    let suffix = common_suffix_len(old, new_text);
    (
        range.start + prefix,
        old[..old.len() - suffix].to_owned(),
        new_text[..new_text.len() - suffix].to_owned(),
    )
}

/// Bytes shared at the start of both strings, backed off to a character
/// boundary so a partially shared code point is not split.
fn common_prefix_len(a: &str, b: &str) -> usize {
    let mut len = a
        .as_bytes()
        .iter()
        .zip(b.as_bytes())
        .take_while(|(x, y)| x == y)
        .count();
    while !a.is_char_boundary(len) {
        len -= 1;
    }
    len
}

/// Bytes shared at the end of both strings. The shared bytes are identical,
/// so a boundary found in one is a boundary in the other.
fn common_suffix_len(a: &str, b: &str) -> usize {
    let mut len = a
        .as_bytes()
        .iter()
        .rev()
        .zip(b.as_bytes().iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    while !a.is_char_boundary(a.len() - len) {
        len -= 1;
    }
    len
}

#[derive(Clone)]
pub enum InputEvent {
    /// Enter — or, in a one-line field, the primary-modifier chord too. The
    /// content is delivered as-is and survives the submission; an owner that
    /// consumes what it submits (the composer) trims and clears itself.
    Submit(String),
    /// The field took focus. A file editor uses this to re-read its file, so
    /// clicking back into it picks up changes made on disk meanwhile.
    Focus,
    /// The text content actually changed. Parents that derive UI from the
    /// content — filter lists, draft state, dirty markers — react to this,
    /// never to raw notifies: the field also notifies for caret blinks and
    /// selection changes, and re-rendering an owner twice a second for a
    /// blinking caret is exactly the per-frame waste the field exists to
    /// contain.
    Edited,
    /// Backspace with nothing left to delete. Owners with something staged
    /// behind the field — the composer's attachments — listen for this;
    /// everyone else ignores it.
    BackspaceOnEmpty,
}

/// Clipboard payloads whose primary representation is an image or file list,
/// emitted instead of a text splice by fields that opted in via
/// [`TextInput::media_paste`]. The composer persists them and presents
/// them as attachment chips.
#[derive(Clone)]
pub struct MediaPaste(pub Vec<ClipboardEntry>);

/// Respect the representation priority chosen by the source application.
/// Finder puts paths first (and a text fallback second), while screenshots put
/// an image first. Text-first clipboard content remains ordinary text paste.
fn media_paste_entries(clipboard: &ClipboardItem) -> Option<Vec<ClipboardEntry>> {
    if !matches!(
        clipboard.entries().first(),
        Some(ClipboardEntry::Image(_) | ClipboardEntry::ExternalPaths(_))
    ) {
        return None;
    }
    let entries = clipboard
        .entries()
        .iter()
        .filter(|entry| {
            matches!(
                entry,
                ClipboardEntry::Image(_) | ClipboardEntry::ExternalPaths(_)
            )
        })
        .cloned()
        .collect::<Vec<_>>();
    (!entries.is_empty()).then_some(entries)
}

/// The web's input/textarea split, because it is the load-bearing one: a
/// single-line field never wraps — overlong text slides horizontally under a
/// clipped viewport, pasted breaks collapse to spaces, and Enter always
/// submits, keeping the content the way a find bar keeps its query — while a
/// multi-line field wraps, keeps pasted breaks, and gives vertical arrows to
/// the caret. Everything else a specialised field wants layers on through
/// explicit options ([`multi_line`](TextInput::multi_line),
/// [`submit_on_enter`](TextInput::submit_on_enter),
/// [`auto_height`](TextInput::auto_height),
/// [`media_paste`](TextInput::media_paste)) or a wrapper component.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum FieldMode {
    #[default]
    SingleLine,
    MultiLine,
}

/// Normalise clipboard text for the field it lands in: a single-line field
/// collapses breaks to spaces, a multi-line field keeps them, with CRLF and
/// lone CR folded to the `\n` the layout shapes its lines on.
fn pasted_text_for_mode(mode: FieldMode, text: &str) -> String {
    match mode {
        FieldMode::SingleLine => text.replace(['\n', '\r'], " "),
        FieldMode::MultiLine => text.replace("\r\n", "\n").replace('\r', "\n"),
    }
}

/// Tallest an [`auto_height`](TextInput::auto_height) field grows before
/// its text scrolls under an overlay scrollbar instead of growing the card.
const AUTO_HEIGHT_MAX: Pixels = px(300.);

/// The shared text engine under every box that takes typing — search inputs,
/// the address bar, file editors, and the composer wrapper: native macOS
/// editing and IME composition, grouped undo history, mouse selection, and
/// paint-only syntax colouring, specialised through config rather than forks.
pub struct TextInput {
    focus_handle: FocusHandle,
    mode: FieldMode,
    read_only: bool,
    /// Enter submits this multi-line field instead of inserting a newline;
    /// Shift+Enter still breaks the line. (One-line fields always submit.)
    submit_on_enter: bool,
    /// The field owns its height and text metrics, growing with its content
    /// up to [`AUTO_HEIGHT_MAX`] before it scrolls; otherwise a
    /// multi-line field inherits the embedding view's metrics.
    auto_height: bool,
    /// Image and file pastes surface as [`MediaPaste`] instead of being
    /// swallowed by the text path.
    accepts_media_paste: bool,
    /// Escape clears the field when it has content; an empty field lets the
    /// keystroke fall through to the surface's own escape.
    clear_on_escape: bool,
    /// The focusing click selects the whole content on release, the way a
    /// browser address bar arms its URL for retyping.
    select_all_on_focus_click: bool,
    /// A plain click landed while the field was unfocused; unless it grows
    /// into a drag-selection first, the release selects everything.
    focus_click_select_all: bool,
    /// Language for paint-only syntax colouring, in code mode.
    language: Option<Lang>,
    /// Cached token spans over `content`, as absolute byte ranges. Recomputed
    /// only when the content changes, so painting a large file is free.
    highlight: Vec<(Range<usize>, TokenClass)>,
    /// Find-in-file match ranges painted as washes under the text, sorted and
    /// non-overlapping. Owned by the find bar, which recomputes them whenever
    /// the content or the query changes; the field only paints them.
    search_matches: Vec<Range<usize>>,
    /// Index into `search_matches` of the match navigation is on, painted
    /// stronger than its siblings.
    active_search_match: Option<usize>,
    content: SharedString,
    placeholder: SharedString,
    selected_range: Range<usize>,
    selection_reversed: bool,
    marked_range: Option<Range<usize>>,
    /// How far a single-line field's text is slid left of its
    /// clipped viewport, in pixels. Reconciled every prepaint to keep the
    /// caret in view; pinned to zero while the field is unfocused so the
    /// address bar's page echo shows the start of the URL.
    scroll_offset: Pixels,
    /// Vertical scroll state for an auto-height field, whose height is
    /// capped at [`AUTO_HEIGHT_MAX`].
    scroll_handle: ScrollHandle,
    scrollbar_state: Rc<ScrollbarState>,
    /// Horizontal inset an auto-height embedder moves inside the field, so
    /// the scroll viewport — and the overlay scrollbar pinned to its edge —
    /// runs to the card's edge while the text keeps the inset.
    padding_x: Pixels,
    /// The `(caret, content length, wrap width)` the capped viewport last
    /// followed. Prepaint scrolls the caret back into view only when this
    /// changes, so a manual wheel scroll away from the caret holds until the
    /// caret itself next moves.
    caret_reconciled: Option<(usize, usize, Pixels)>,
    last_layout: Option<TextLayout>,
    /// Horizontal goal and soft-wrap affinity for consecutive Up/Down
    /// presses. A byte offset at a wrap boundary can mean either the end of
    /// one visual row or the start of the next, so the offset alone is not
    /// enough to reproduce native textarea movement or paint its caret.
    vertical_navigation: Option<VerticalNavigation>,
    is_selecting: bool,
    selected_word_range: Option<Range<usize>>,
    history: EditHistory,
    external_context_menu_focus_holds: usize,
    context_menu: ContextMenuHandle,
    blink_cursor: Entity<BlinkCursor>,
    _subscriptions: Vec<Subscription>,
}

#[derive(Clone, Copy)]
struct VerticalNavigation {
    /// Desired horizontal position within a visual row. This survives a
    /// shorter intermediate row so moving again can return to the old column.
    goal_x: Pixels,
    /// The visual row carrying the caret, including soft-wrapped rows.
    visual_row: usize,
    /// Actual caret x within `visual_row`, which can be less than `goal_x`
    /// when that row is shorter.
    cursor_x: Pixels,
    cursor_offset: usize,
    layout_width: Pixels,
}

impl TextInput {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        let blink_cursor = cx.new(|_| BlinkCursor::new());
        let _subscriptions = vec![
            cx.observe(&blink_cursor, |_, _, cx| cx.notify()),
            cx.observe_window_activation(window, |input, window, cx| {
                if window.is_window_active()
                    && (input.focus_handle.is_focused(window)
                        || input.context_menu_preserves_visual_focus())
                {
                    input.blink_cursor.update(cx, |cursor, cx| cursor.start(cx));
                } else if !window.is_window_active() {
                    input.blink_cursor.update(cx, |cursor, cx| cursor.stop(cx));
                }
            }),
            cx.on_focus(&focus_handle, window, Self::on_focus),
            cx.on_blur(&focus_handle, window, Self::on_blur),
        ];
        Self {
            focus_handle,
            mode: FieldMode::SingleLine,
            read_only: false,
            submit_on_enter: false,
            auto_height: false,
            accepts_media_paste: false,
            clear_on_escape: false,
            select_all_on_focus_click: false,
            focus_click_select_all: false,
            language: None,
            highlight: Vec::new(),
            search_matches: Vec::new(),
            active_search_match: None,
            content: "".into(),
            placeholder: "".into(),
            selected_range: 0..0,
            selection_reversed: false,
            marked_range: None,
            scroll_offset: px(0.),
            scroll_handle: ScrollHandle::new(),
            scrollbar_state: ScrollbarState::new(),
            padding_x: px(0.),
            caret_reconciled: None,
            last_layout: None,
            vertical_navigation: None,
            is_selecting: false,
            selected_word_range: None,
            history: EditHistory::default(),
            external_context_menu_focus_holds: 0,
            context_menu: {
                // The menu takes real focus while open, so the composer holds
                // its caret visible for the duration — otherwise right-clicking
                // the input looks like it defocused.
                let composer = cx.entity().downgrade();
                ContextMenuHandle::new(cx).on_toggle(move |open, window, cx| {
                    let _ = composer.update(cx, |composer: &mut Self, cx| {
                        if open {
                            composer.preserve_visual_focus_for_context_menu(window, cx);
                        } else {
                            composer.release_visual_focus_for_context_menu(window, cx);
                        }
                    });
                })
            },
            blink_cursor,
            _subscriptions,
        }
    }

    pub fn focus(&self) -> FocusHandle {
        self.focus_handle.clone()
    }

    pub fn is_visually_focused(&self, window: &Window) -> bool {
        self.focus_handle.is_focused(window) || self.context_menu_preserves_visual_focus()
    }

    pub fn preserve_visual_focus_for_context_menu(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.is_visually_focused(window) {
            return false;
        }
        self.external_context_menu_focus_holds += 1;
        cx.notify();
        true
    }

    pub fn release_visual_focus_for_context_menu(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        self.external_context_menu_focus_holds =
            self.external_context_menu_focus_holds.saturating_sub(1);
        if !self.is_visually_focused(window) {
            self.blink_cursor.update(cx, |cursor, cx| cursor.stop(cx));
        }
        cx.notify();
    }

    /// Whether this field's right-click menu is open. The browser surface
    /// treats that as an overlay above its native webview.
    pub fn context_menu_open(&self) -> bool {
        self.context_menu.is_open()
    }

    fn context_menu_preserves_visual_focus(&self) -> bool {
        self.external_context_menu_focus_holds > 0
    }

    /// Placeholder shown while the field is empty.
    pub fn placeholder(mut self, placeholder: impl Into<SharedString>) -> Self {
        self.placeholder = placeholder.into();
        self
    }

    /// Replace the placeholder after construction. Picker fields use this to
    /// name the workspace they are searching without recreating the focused
    /// input entity whenever the selected project changes.
    pub fn set_placeholder(
        &mut self,
        placeholder: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) {
        let placeholder = placeholder.into();
        if self.placeholder != placeholder {
            self.placeholder = placeholder;
            cx.notify();
        }
    }

    /// Let the text wrap onto multiple lines: Enter inserts a newline unless
    /// [`submit_on_enter`](Self::submit_on_enter) rebinds it, and pasted
    /// breaks survive. The field inherits the caller's text metrics, so a
    /// gutter beside an editor can rely on the same line height.
    pub fn multi_line(mut self) -> Self {
        self.mode = FieldMode::MultiLine;
        self
    }

    /// Paint-only syntax colouring for `language`, when recognised.
    pub fn syntax(mut self, language: Option<&str>) -> Self {
        self.language = language.and_then(highlight::lang_for_tag);
        self
    }

    /// Make Enter submit this multi-line field instead of inserting a
    /// newline; Shift+Enter still breaks the line.
    pub fn submit_on_enter(mut self) -> Self {
        self.submit_on_enter = true;
        self
    }

    /// Let the field own its height and text metrics: it grows with its
    /// content up to [`AUTO_HEIGHT_MAX`], then scrolls under an overlay
    /// scrollbar that follows the caret.
    pub fn auto_height(mut self) -> Self {
        self.auto_height = true;
        self
    }

    /// Surface image and file pastes as a [`MediaPaste`] event instead of
    /// swallowing them; text-first clipboards still paste as text.
    pub fn media_paste(mut self) -> Self {
        self.accepts_media_paste = true;
        self
    }

    /// Make Escape clear the field first, the filter-field convention: only
    /// a second press on the emptied field reaches the surface's own escape
    /// (dismissing the popover or palette around it).
    pub fn clear_on_escape(mut self) -> Self {
        self.clear_on_escape = true;
        self
    }

    /// Horizontal inset kept inside the field's scroll viewport rather than
    /// on the embedding card, so the overlay scrollbar sits at the card's
    /// edge instead of floating next to the text.
    pub fn set_padding_x(&mut self, padding: Pixels) {
        self.padding_x = padding;
    }

    /// Make the focusing click select the whole content on release, the way a
    /// browser address bar arms its URL for retyping. A drag from unfocused
    /// still selects the dragged range, and the next click places the caret.
    pub fn select_all_on_focus_click(mut self) -> Self {
        self.select_all_on_focus_click = true;
        self
    }

    /// Reject edits while still allowing selection and copy.
    pub fn read_only(mut self, read_only: bool) -> Self {
        self.read_only = read_only;
        self
    }

    /// Flip read-only after construction. A file editor starts locked because
    /// its contents are still being read off the UI thread, and unlocks once
    /// the read lands and says the file is writable.
    pub fn set_read_only(&mut self, read_only: bool) {
        self.read_only = read_only;
    }

    /// Re-tokenize after a content change. Cheap for a composer (no language),
    /// one linear pass for a code editor.
    fn refresh_highlight(&mut self) {
        let Some(language) = self.language else {
            return;
        };
        self.highlight.clear();
        let mut line_start = 0;
        for (line, tokens) in self
            .content
            .split('\n')
            .zip(highlight::tokenize(language, &self.content))
        {
            self.highlight.extend(tokens.into_iter().map(|token| {
                (
                    line_start + token.range.start..line_start + token.range.end,
                    token.class,
                )
            }));
            line_start += line.len() + 1;
        }
    }

    pub fn content(&self) -> &str {
        &self.content
    }

    /// The caret's byte offset into `content`.
    pub fn cursor(&self) -> usize {
        self.cursor_offset()
    }

    /// Splice `text` over `range` and put the caret after it. This is the
    /// autocompletion insert: unlike [`EntityInputHandler::replace_text_in_range`]
    /// it takes byte offsets and no window, so an action handler can call it.
    pub fn replace_range(&mut self, range: Range<usize>, text: &str, cx: &mut Context<Self>) {
        if self.read_only {
            return;
        }
        let range = range.start.min(self.content.len())..range.end.min(self.content.len());
        if !self.content.is_char_boundary(range.start)
            || !self.content.is_char_boundary(range.end)
            || range.start > range.end
        {
            return;
        }
        // A discrete undo step: picking a completion or replacing a match
        // must not coalesce with the typing around it.
        self.history.seal();
        self.record_edit_history(&range, text, false);
        self.content =
            (self.content[..range.start].to_owned() + text + &self.content[range.end..]).into();
        let offset = range.start + text.len();
        self.selected_range = offset..offset;
        self.selection_reversed = false;
        self.marked_range = None;
        self.vertical_navigation = None;
        self.history.seal();
        self.refresh_highlight();
        self.pause_blink_cursor(cx);
        cx.emit(InputEvent::Edited);
        cx.notify();
    }

    /// Replace the painted find-match washes. Ranges must be sorted and
    /// non-overlapping; `active` indexes into `matches`. Purely visual — the
    /// content is untouched, so no [`InputEvent::Edited`] is emitted.
    pub fn set_search_matches(
        &mut self,
        matches: Vec<Range<usize>>,
        active: Option<usize>,
        cx: &mut Context<Self>,
    ) {
        if self.search_matches == matches && self.active_search_match == active {
            return;
        }
        self.search_matches = matches;
        self.active_search_match = active;
        cx.notify();
    }

    pub fn selected_range(&self) -> Range<usize> {
        self.selected_range.clone()
    }

    /// Move the selection to `range`, as find-next does when it lands on a
    /// match. Ignored unless the range sits on character boundaries, so a
    /// selection computed against stale content cannot split a code point.
    pub fn select_range(&mut self, range: Range<usize>, cx: &mut Context<Self>) {
        if range.start > range.end
            || range.end > self.content.len()
            || !self.content.is_char_boundary(range.start)
            || !self.content.is_char_boundary(range.end)
        {
            return;
        }
        self.selected_range = range;
        self.selection_reversed = false;
        self.vertical_navigation = None;
        self.pause_blink_cursor(cx);
        cx.notify();
    }

    /// Select the whole content, the way reopening a find bar re-arms its
    /// query for retyping. Unlike the `SelectAll` action this needs no window.
    pub fn select_all_text(&mut self, cx: &mut Context<Self>) {
        self.selected_range = 0..self.content.len();
        self.selection_reversed = false;
        self.vertical_navigation = None;
        self.pause_blink_cursor(cx);
        cx.notify();
    }

    /// Where `offset` sits in the window as of the last paint, with the line
    /// height, so a find bar can scroll a match into view. `None` until the
    /// field has painted once.
    pub fn position_for_offset(&self, offset: usize) -> Option<(Point<Pixels>, Pixels)> {
        let layout = self.last_layout.as_ref()?;
        let position = layout.position_for_index(offset.min(self.content.len()))?;
        Some((position, layout.line_height()))
    }

    /// Height of each logical line as laid out, so a gutter can put one number
    /// per line even when soft wrap gives a line several visual rows.
    ///
    /// Read from the previous frame's layout — a gutter is therefore one frame
    /// behind a reflow, which is invisible next to the reflow itself.
    pub fn wrapped_line_heights(&self) -> Vec<Pixels> {
        let Some(layout) = self.last_layout.as_ref() else {
            return Vec::new();
        };
        let line_height = layout.line_height();
        layout
            .line_layouts()
            .iter()
            .map(|line| line_height * (line.wrap_boundaries().len() + 1) as f32)
            .collect()
    }

    pub fn clear(&mut self, cx: &mut Context<Self>) {
        let changed = !self.content.is_empty();
        self.content = "".into();
        self.selected_range = 0..0;
        self.selection_reversed = false;
        self.marked_range = None;
        self.vertical_navigation = None;
        self.highlight.clear();
        // A programmatic clear is a new baseline, not an edit to step back
        // over — a submitted prompt should not resurface via the undo shortcut.
        if changed {
            self.history = EditHistory::default();
        }
        self.pause_blink_cursor(cx);
        if changed {
            cx.emit(InputEvent::Edited);
        }
        cx.notify();
    }

    pub fn set_content(&mut self, content: impl Into<SharedString>, cx: &mut Context<Self>) {
        let content = content.into();
        let changed = self.content != content;
        self.content = content;
        let offset = self.content.len();
        self.selected_range = offset..offset;
        self.selection_reversed = false;
        self.marked_range = None;
        self.vertical_navigation = None;
        // A load or reload from disk is a new baseline: undoing into text
        // from before an external change would silently revert that change.
        // An unchanged reload keeps the history alive.
        if changed {
            self.history = EditHistory::default();
        }
        self.refresh_highlight();
        self.pause_blink_cursor(cx);
        if changed {
            cx.emit(InputEvent::Edited);
        }
        cx.notify();
    }

    fn on_focus(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        // Regaining focus is a gesture boundary — Zed finalizes its last
        // transaction here too — so edits from separate visits never merge
        // into one undo step.
        self.history.seal();
        self.blink_cursor.update(cx, |cursor, cx| cursor.start(cx));
        cx.emit(InputEvent::Focus);
    }

    fn on_blur(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        // An armed focusing click dies with the focus it was tied to.
        self.focus_click_select_all = false;
        if self.context_menu_preserves_visual_focus() {
            cx.notify();
            return;
        }
        self.blink_cursor.update(cx, |cursor, cx| cursor.stop(cx));
    }

    fn pause_blink_cursor(&mut self, cx: &mut Context<Self>) {
        self.blink_cursor.update(cx, |cursor, cx| cursor.pause(cx));
    }

    fn left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.previous_boundary(self.cursor_offset()), cx);
        } else {
            self.move_to(self.selected_range.start, cx);
        }
    }

    fn right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.next_boundary(self.cursor_offset()), cx);
        } else {
            self.move_to(self.selected_range.end, cx);
        }
    }

    fn up(&mut self, _: &Up, _: &mut Window, cx: &mut Context<Self>) {
        self.move_vertically(false, cx);
    }

    fn down(&mut self, _: &Down, _: &mut Window, cx: &mut Context<Self>) {
        self.move_vertically(true, cx);
    }

    /// Move by one rendered row, not merely one newline-delimited line. This
    /// is the textarea convention: soft wraps count, and the original x goal
    /// survives a shorter row between two longer ones.
    fn move_vertically(&mut self, down: bool, cx: &mut Context<Self>) {
        if self.mode == FieldMode::SingleLine {
            self.vertical_navigation = None;
            cx.propagate();
            return;
        }

        let anchor = if down {
            self.selected_range.end
        } else {
            self.selected_range.start
        };
        let Some(layout) = self
            .last_layout
            .as_ref()
            .filter(|layout| layout.len() == self.content.len())
        else {
            // The field normally has a current layout whenever it can receive
            // a key. If an edit and this action race the next paint, let an
            // enclosing surface handle the arrow rather than navigating with
            // stale geometry.
            self.vertical_navigation = None;
            cx.propagate();
            return;
        };
        let row_count = visual_row_count(layout);
        if row_count == 0 {
            cx.propagate();
            return;
        }

        let bounds = layout.bounds();
        let layout_width = bounds.size.width;
        let continuing = if self.selected_range.is_empty() {
            self.vertical_navigation.filter(|navigation| {
                navigation.cursor_offset == anchor
                    && navigation.layout_width == layout_width
                    && navigation.visual_row < row_count
            })
        } else {
            None
        };
        let (current_row, goal_x) = if let Some(navigation) = continuing {
            (navigation.visual_row, navigation.goal_x)
        } else {
            let Some(position) = layout.position_for_index(anchor) else {
                self.vertical_navigation = None;
                cx.propagate();
                return;
            };
            let row = ((position.y - bounds.top()) / layout.line_height()) as usize;
            (row.min(row_count - 1), position.x - bounds.left())
        };
        let target_row = if down {
            (current_row + 1).min(row_count - 1)
        } else {
            current_row.saturating_sub(1)
        };
        let Some((offset, cursor_x)) = visual_row_offset_for_x(layout, target_row, goal_x) else {
            self.vertical_navigation = None;
            cx.propagate();
            return;
        };

        let previous_range = self.selected_range.clone();
        let previous_row = continuing.map(|navigation| navigation.visual_row);
        self.selected_range = offset..offset;
        self.selection_reversed = false;
        self.vertical_navigation = Some(VerticalNavigation {
            goal_x,
            visual_row: target_row,
            cursor_x,
            cursor_offset: offset,
            layout_width,
        });
        self.pause_blink_cursor(cx);
        cx.notify();

        // Match Zed/native controls at the boundary: if neither the text
        // selection nor its soft-wrap affinity moved, an enclosing surface
        // gets a chance to use the arrow.
        if previous_range == self.selected_range && previous_row == Some(target_row) {
            cx.propagate();
        }
    }

    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.previous_boundary(self.cursor_offset()), cx);
    }

    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.next_boundary(self.cursor_offset()), cx);
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
        self.select_to(self.content.len(), cx);
    }

    fn home(&mut self, _: &Home, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
    }

    fn end(&mut self, _: &End, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.content.len(), cx);
    }

    fn move_to_previous_word(
        &mut self,
        _: &MoveToPreviousWord,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let offset = if self.selected_range.is_empty() {
            previous_word_boundary(&self.content, self.cursor_offset())
        } else {
            self.selected_range.start
        };
        self.move_to(offset, cx);
    }

    fn move_to_next_word(&mut self, _: &MoveToNextWord, _: &mut Window, cx: &mut Context<Self>) {
        let offset = if self.selected_range.is_empty() {
            next_word_boundary(&self.content, self.cursor_offset())
        } else {
            self.selected_range.end
        };
        self.move_to(offset, cx);
    }

    fn select_to_start(&mut self, _: &SelectToStart, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(0, cx);
    }

    fn select_to_end(&mut self, _: &SelectToEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.content.len(), cx);
    }

    fn select_to_previous_word(
        &mut self,
        _: &SelectToPreviousWord,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_to(
            previous_word_boundary(&self.content, self.cursor_offset()),
            cx,
        );
    }

    fn select_to_next_word(
        &mut self,
        _: &SelectToNextWord,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_to(next_word_boundary(&self.content, self.cursor_offset()), cx);
    }

    fn backspace(&mut self, _: &Backspace, window: &mut Window, cx: &mut Context<Self>) {
        if self.content.is_empty() {
            // Nothing to delete; owners with something staged behind the
            // field (the composer's attachments) listen for this instead.
            cx.emit(InputEvent::BackspaceOnEmpty);
            return;
        }
        if self.selected_range.is_empty() {
            self.select_to(self.previous_boundary(self.cursor_offset()), cx);
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    fn delete(&mut self, _: &Delete, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.select_to(self.next_boundary(self.cursor_offset()), cx);
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    fn delete_to_start(&mut self, _: &DeleteToStart, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.select_to(0, cx);
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    fn delete_to_end(&mut self, _: &DeleteToEnd, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.select_to(self.content.len(), cx);
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    fn delete_to_previous_word(
        &mut self,
        _: &DeleteToPreviousWord,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.selected_range.is_empty() {
            self.select_to(
                previous_word_boundary(&self.content, self.cursor_offset()),
                cx,
            );
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    fn delete_to_next_word(
        &mut self,
        _: &DeleteToNextWord,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.selected_range.is_empty() {
            self.select_to(next_word_boundary(&self.content, self.cursor_offset()), cx);
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    fn enter(&mut self, _: &Enter, window: &mut Window, cx: &mut Context<Self>) {
        if self.mode == FieldMode::MultiLine && !self.submit_on_enter {
            self.replace_text_in_range(None, "\n", window, cx);
            return;
        }
        // The content survives its own submission — a find bar's Enter means
        // "find next", not "send" — and goes out untrimmed because leading or
        // trailing spaces are part of what is searched for. An owner that
        // consumes what it submits (the composer wrapper) trims and clears.
        cx.emit(InputEvent::Submit(self.content.to_string()));
    }

    fn newline(&mut self, _: &Newline, window: &mut Window, cx: &mut Context<Self>) {
        if self.mode == FieldMode::SingleLine {
            // Find bars and picker fields assign Shift+Enter their own meaning.
            cx.propagate();
            return;
        }
        self.replace_text_in_range(None, "\n", window, cx);
    }

    fn clear_field(&mut self, _: &Clear, _: &mut Window, cx: &mut Context<Self>) {
        if !self.clear_on_escape || self.content.is_empty() {
            cx.propagate();
            return;
        }
        self.clear(cx);
    }

    fn submit_steer(&mut self, _: &SubmitSteer, _: &mut Window, cx: &mut Context<Self>) {
        match self.mode {
            // For a one-line field the forceful chord is just Enter.
            FieldMode::SingleLine => {
                cx.emit(InputEvent::Submit(self.content.to_string()));
            }
            // Steering is prompt vocabulary. The field only propagates the
            // action; the composer wrapper claims it from an outer handler.
            FieldMode::MultiLine => cx.propagate(),
        }
    }

    fn paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        let Some(clipboard) = cx.read_from_clipboard() else {
            return;
        };
        if self.accepts_media_paste
            && let Some(entries) = media_paste_entries(&clipboard)
        {
            cx.emit(MediaPaste(entries));
            return;
        }
        let Some(text) = clipboard.text() else {
            return;
        };
        let text = pasted_text_for_mode(self.mode, &text);
        // A paste is its own undo step, never part of the typing around it —
        // the native NSTextView boundary, stricter than Zed's time grouping.
        self.history.seal();
        self.replace_text_in_range(None, &text, window, cx);
        self.history.seal();
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            // Nothing here to copy. The composer holds focus almost all the
            // time, so propagating lets an outer handler — the transcript's
            // text selection — answer the keystroke instead of swallowing it.
            cx.propagate();
            return;
        }
        cx.write_to_clipboard(ClipboardItem::new_string(
            self.content[self.selected_range.clone()].to_string(),
        ));
    }

    fn cut(&mut self, _: &Cut, window: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.content[self.selected_range.clone()].to_string(),
            ));
            // Like paste, a cut never coalesces with surrounding deletions.
            self.history.seal();
            self.replace_text_in_range(None, "", window, cx);
            self.history.seal();
        }
    }

    /// Route a splice into the history before it is applied: composition
    /// splices amend the open composition step, everything else records —
    /// and possibly coalesces — normally.
    fn record_edit_history(&mut self, range: &Range<usize>, new_text: &str, composing: bool) {
        if composing {
            self.history.record_composition(
                &self.content,
                range,
                new_text,
                self.selected_range.clone(),
                self.selection_reversed,
                Instant::now(),
            );
        } else {
            self.history.record(
                &self.content,
                range,
                new_text,
                self.selected_range.clone(),
                self.selection_reversed,
                Instant::now(),
            );
        }
    }

    fn undo(&mut self, _: &Undo, _: &mut Window, cx: &mut Context<Self>) {
        // While text is marked the IME owns the field; undoing under an
        // active composition would desync it.
        if self.read_only || self.marked_range.is_some() {
            return;
        }
        let Some((content, selection, selection_reversed)) = self.history.undo(&self.content)
        else {
            return;
        };
        self.apply_history_step(content, selection, selection_reversed, cx);
    }

    fn redo(&mut self, _: &Redo, _: &mut Window, cx: &mut Context<Self>) {
        if self.read_only || self.marked_range.is_some() {
            return;
        }
        let Some((content, selection, selection_reversed)) = self.history.redo(&self.content)
        else {
            return;
        };
        self.apply_history_step(content, selection, selection_reversed, cx);
    }

    fn apply_history_step(
        &mut self,
        content: String,
        selection: Range<usize>,
        selection_reversed: bool,
        cx: &mut Context<Self>,
    ) {
        self.content = content.into();
        self.selected_range = selection;
        self.selection_reversed = selection_reversed;
        self.marked_range = None;
        self.vertical_navigation = None;
        self.refresh_highlight();
        self.pause_blink_cursor(cx);
        cx.emit(InputEvent::Edited);
        cx.notify();
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.vertical_navigation = None;
        self.is_selecting = true;
        self.selected_word_range = None;
        // A plain click that is also the focusing click arms select-all for
        // its release. This handler runs before gpui's focus-on-mouse-down
        // transfer (user listeners dispatch first in the bubble phase), so
        // the pre-click focus is still observable here.
        self.focus_click_select_all = self.select_all_on_focus_click
            && event.click_count == 1
            && !event.modifiers.shift
            && !self.is_visually_focused(window);
        let offset = self.index_for_mouse_position(event.position);

        if event.click_count >= 3 {
            self.selected_range = 0..self.content.len();
            self.selection_reversed = false;
            self.selected_word_range = Some(self.selected_range.clone());
            self.pause_blink_cursor(cx);
            cx.notify();
            return;
        }

        if event.click_count == 2 {
            let range = word_range_at(&self.content, offset);
            self.selected_range = range.clone();
            self.selection_reversed = false;
            self.selected_word_range = (!range.is_empty()).then_some(range);
            self.pause_blink_cursor(cx);
            cx.notify();
            return;
        }

        if event.modifiers.shift {
            self.select_to(offset, cx);
        } else {
            self.move_to(offset, cx);
        }
    }

    fn on_context_mouse_down(
        &mut self,
        _: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle, cx);
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.is_selecting = false;
        self.selected_word_range = None;
        if self.focus_click_select_all {
            self.focus_click_select_all = false;
            self.select_all_text(cx);
        }
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.is_selecting {
            self.select_to(self.index_for_mouse_position(event.position), cx);
            // Growing a real drag-selection turns the focusing click into a
            // range selection; releasing must keep it.
            if !self.selected_range.is_empty() {
                self.focus_click_select_all = false;
            }
        }
    }

    fn move_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.selected_range = offset..offset;
        self.selection_reversed = false;
        self.vertical_navigation = None;
        self.pause_blink_cursor(cx);
        cx.notify();
    }

    fn cursor_offset(&self) -> usize {
        if self.selection_reversed {
            self.selected_range.start
        } else {
            self.selected_range.end
        }
    }

    fn index_for_mouse_position(&self, position: Point<Pixels>) -> usize {
        if self.content.is_empty() {
            return 0;
        }
        let Some(layout) = self.last_layout.as_ref() else {
            return 0;
        };
        layout
            .index_for_position(position)
            .unwrap_or_else(|index| index)
            .min(self.content.len())
    }

    fn select_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.vertical_navigation = None;
        if self.selection_reversed {
            self.selected_range.start = offset;
        } else {
            self.selected_range.end = offset;
        }
        if self.selected_range.end < self.selected_range.start {
            self.selection_reversed = !self.selection_reversed;
            self.selected_range = self.selected_range.end..self.selected_range.start;
        }
        if let Some(word_range) = self.selected_word_range.as_ref() {
            self.selected_range.start = self.selected_range.start.min(word_range.start);
            self.selected_range.end = self.selected_range.end.max(word_range.end);
        }
        self.pause_blink_cursor(cx);
        cx.notify();
    }

    fn offset_from_utf16(&self, offset: usize) -> usize {
        let mut utf8_offset = 0;
        let mut utf16_count = 0;
        for character in self.content.chars() {
            if utf16_count >= offset {
                break;
            }
            utf16_count += character.len_utf16();
            utf8_offset += character.len_utf8();
        }
        utf8_offset
    }

    fn offset_to_utf16(&self, offset: usize) -> usize {
        let mut utf16_offset = 0;
        let mut utf8_count = 0;
        for character in self.content.chars() {
            if utf8_count >= offset {
                break;
            }
            utf8_count += character.len_utf8();
            utf16_offset += character.len_utf16();
        }
        utf16_offset
    }

    fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }

    fn range_from_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(range.start)..self.offset_from_utf16(range.end)
    }

    /// Resolves a range whose endpoints count UTF-16 units from `base`, the
    /// form macOS uses for everything relative to the marked text. The offsets
    /// have to be added in UTF-16 before the conversion; converting first and
    /// adding to `base` overshoots once anything multi-byte precedes it.
    fn range_from_relative_utf16(&self, base: usize, range: &Range<usize>) -> Range<usize> {
        let base_utf16 = self.offset_to_utf16(base);
        self.range_from_utf16(&(base_utf16 + range.start..base_utf16 + range.end))
    }

    fn previous_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .rev()
            .find_map(|(index, _)| (index < offset).then_some(index))
            .unwrap_or(0)
    }

    fn next_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .find_map(|(index, _)| (index > offset).then_some(index))
            .unwrap_or(self.content.len())
    }
}

fn previous_word_boundary(content: &str, offset: usize) -> usize {
    content[..offset]
        .split_word_bound_indices()
        .rev()
        .find(|(_, segment)| !segment.chars().all(char::is_whitespace))
        .map(|(index, _)| index)
        .unwrap_or(0)
}

fn next_word_boundary(content: &str, offset: usize) -> usize {
    content[offset..]
        .split_word_bound_indices()
        .find(|(_, segment)| !segment.chars().all(char::is_whitespace))
        .map(|(index, segment)| offset + index + segment.len())
        .unwrap_or(content.len())
}

fn word_range_at(content: &str, offset: usize) -> Range<usize> {
    content
        .split_word_bound_indices()
        .find_map(|(index, segment)| {
            let range = index..index + segment.len();
            range.contains(&offset).then_some(range)
        })
        .unwrap_or(offset..offset)
}

impl EventEmitter<InputEvent> for TextInput {}
impl EventEmitter<MediaPaste> for TextInput {}

impl EntityInputHandler for TextInput {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.range_from_utf16(&range_utf16);
        actual_range.replace(self.range_to_utf16(&range));
        Some(self.content[range].to_string())
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.range_to_utf16(&self.selected_range),
            reversed: self.selection_reversed,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked_range
            .as_ref()
            .map(|range| self.range_to_utf16(range))
    }

    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {
        self.marked_range = None;
        self.history.finalize_composition();
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.read_only {
            return;
        }
        // While text is marked, macOS reports replacement ranges relative to
        // the marked text, so the marked span itself is the commit target —
        // Zed's reading of the protocol. Absolute ranges only arrive outside
        // composition (e.g. the Accessibility Keyboard's completions).
        let composing = self.marked_range.is_some();
        let range = self.marked_range.clone().unwrap_or_else(|| {
            range_utf16
                .as_ref()
                .map(|range| self.range_from_utf16(range))
                .unwrap_or(self.selected_range.clone())
        });
        self.record_edit_history(&range, new_text, composing);
        let previous = self.content.clone();
        self.content =
            (self.content[..range.start].to_owned() + new_text + &self.content[range.end..]).into();
        let offset = range.start + new_text.len();
        self.selected_range = offset..offset;
        self.marked_range = None;
        self.vertical_navigation = None;
        if composing {
            self.history.finalize_composition();
        }
        self.refresh_highlight();
        self.pause_blink_cursor(cx);
        if previous != self.content {
            cx.emit(InputEvent::Edited);
        }
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.read_only {
            return;
        }
        // A range arriving while text is marked is relative to the marked
        // text, and clipped to it the way Zed clips; only without marked
        // text is it absolute.
        let range = match (range_utf16.as_ref(), self.marked_range.as_ref()) {
            (Some(range_utf16), Some(marked)) => {
                let absolute = self.range_from_relative_utf16(marked.start, range_utf16);
                absolute.start.clamp(marked.start, marked.end)
                    ..absolute.end.clamp(marked.start, marked.end)
            }
            (Some(range_utf16), None) => self.range_from_utf16(range_utf16),
            (None, Some(marked)) => marked.clone(),
            (None, None) => self.selected_range.clone(),
        };
        self.record_edit_history(&range, new_text, true);
        let previous = self.content.clone();
        self.content =
            (self.content[..range.start].to_owned() + new_text + &self.content[range.end..]).into();
        self.marked_range =
            (!new_text.is_empty()).then_some(range.start..range.start + new_text.len());
        // Empty composition text is a cancel; close its undo step so a
        // netted-out composition leaves no trace.
        if self.marked_range.is_none() {
            self.history.finalize_composition();
        }
        // The composition's selection is also relative to the marked text,
        // which now starts at `range.start`.
        self.selected_range = new_selected_range_utf16
            .as_ref()
            .map(|new_range| self.range_from_relative_utf16(range.start, new_range))
            .unwrap_or_else(|| {
                let offset = range.start + new_text.len();
                offset..offset
            });
        self.vertical_navigation = None;
        self.pause_blink_cursor(cx);
        if previous != self.content {
            cx.emit(InputEvent::Edited);
        }
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let layout = self.last_layout.as_ref()?;
        let range = self.range_from_utf16(&range_utf16);
        let start = layout.position_for_index(range.start)?;
        let end = layout.position_for_index(range.end)?;
        let line_height = layout.line_height();
        if start.y == end.y {
            Some(Bounds::from_corners(
                start,
                point(end.x, end.y + line_height),
            ))
        } else {
            Some(Bounds::from_corners(
                point(bounds.left(), start.y),
                point(bounds.right(), end.y + line_height),
            ))
        }
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        let layout = self.last_layout.as_ref()?;
        let utf8_index = layout
            .index_for_position(point)
            .unwrap_or_else(|index| index)
            .min(self.content.len());
        Some(self.offset_to_utf16(utf8_index))
    }
}

fn visual_row_count(layout: &TextLayout) -> usize {
    layout
        .line_layouts()
        .iter()
        .map(|line| line.wrap_boundaries().len() + 1)
        .sum()
}

/// Resolve the closest caret offset on one rendered row for a desired x.
/// GPUI's whole-text `position_for_index` intentionally gives a soft-wrap
/// boundary to the preceding row, so this works against the concrete wrapped
/// row and returns its unambiguous caret x as well as the byte offset.
fn visual_row_offset_for_x(
    layout: &TextLayout,
    visual_row: usize,
    goal_x: Pixels,
) -> Option<(usize, Pixels)> {
    let line_height = layout.line_height();
    let mut first_visual_row = 0;
    let mut first_byte = 0;

    for line in layout.line_layouts() {
        let line_row_count = line.wrap_boundaries().len() + 1;
        if visual_row < first_visual_row + line_row_count {
            let row_in_line = visual_row - first_visual_row;
            let local_offset = line
                .closest_index_for_position(
                    point(goal_x, line_height * (row_in_line as f32 + 0.5)),
                    line_height,
                )
                .unwrap_or_else(|offset| offset);
            let row_start = row_in_line
                .checked_sub(1)
                .and_then(|boundary_index| line.wrap_boundaries().get(boundary_index))
                .map(|boundary| {
                    line.unwrapped_layout.runs[boundary.run_ix].glyphs[boundary.glyph_ix].index
                })
                .unwrap_or(0);
            let cursor_x = line.unwrapped_layout.x_for_index(local_offset)
                - line.unwrapped_layout.x_for_index(row_start);
            return Some((first_byte + local_offset, cursor_x));
        }
        first_visual_row += line_row_count;
        // TextLayout separates its newline-delimited shaped lines by the one
        // source byte occupied by `\n`.
        first_byte += line.len() + 1;
    }
    None
}

fn cursor_should_be_visible(
    window_active: bool,
    input_focused: bool,
    context_menu_preserves_focus: bool,
    blink_visible: bool,
) -> bool {
    window_active && (context_menu_preserves_focus || (input_focused && blink_visible))
}

/// Horizontal scroll for a focused single-line field, reconciled every frame
/// against the selection the way Zed's `autoscroll_horizontally` resolves an
/// autoscroll request. `start_x`/`head_x`/`end_x` are text-relative pixel
/// positions of the selection edges (all equal for a caret), `em` is one
/// character advance of lookahead kept past the caret, and `previous` is last
/// frame's scroll, moved as little as possible:
/// - a caret, or a selection that fits, is revealed whole plus the lookahead;
/// - a partial selection too wide to fit follows its head, the way a native
///   field tracks shift+End;
/// - a whole-content selection holds still — native `selectAll` never moves
///   the view, which keeps the focus shortcut on the address bar showing the host.
fn single_line_scroll(
    previous: Pixels,
    viewport: Pixels,
    em: Pixels,
    text_width: Pixels,
    (start_x, head_x, end_x): (Pixels, Pixels, Pixels),
    whole_content_selected: bool,
) -> Pixels {
    let max_scroll = (text_width + em - viewport).max(px(0.));
    let scroll = previous.min(max_scroll).max(px(0.));
    let (target_left, target_right) = if end_x - start_x + em <= viewport {
        (start_x, end_x + em)
    } else if whole_content_selected {
        return scroll;
    } else {
        (head_x, head_x + em)
    };
    if target_left < scroll {
        target_left
    } else if target_right > scroll + viewport {
        target_right - viewport
    } else {
        scroll
    }
}

/// Vertical analogue of [`single_line_scroll`] for a composer-mode field
/// capped at [`AUTO_HEIGHT_MAX`]: scroll the viewport the minimum needed
/// to keep the caret inside it. `caret_top` is the caret's window position in
/// this frame's already-scrolled layout, and the container consumed this
/// frame's offset before prepainting children, so a correction lands on the
/// next frame — which this requests.
fn follow_caret(
    caret_top: Point<Pixels>,
    line_height: Pixels,
    scroll_handle: &ScrollHandle,
    window: &mut Window,
) {
    let viewport = scroll_handle.bounds();
    if viewport.size.height <= px(0.) {
        return;
    }
    let offset = scroll_handle.offset();
    let mut y = offset.y;
    let caret_bottom = caret_top.y + line_height;
    if caret_bottom > viewport.bottom() {
        y -= caret_bottom - viewport.bottom();
    } else if caret_top.y < viewport.top() {
        y += viewport.top() - caret_top.y;
    }
    let y = y.clamp(-scroll_handle.max_offset().y, px(0.));
    if (y - offset.y).abs() > px(0.5) {
        scroll_handle.set_offset(point(offset.x, y));
        window.request_animation_frame();
    }
}

struct InputElement {
    input: Entity<TextInput>,
}

impl InputElement {
    /// For a single-line field, the bounds the text is actually laid out
    /// at: the unwrapped line anchored `scroll_offset` left of the
    /// clipped viewport so the caret stays in view. `None` for wrapping
    /// fields, which lay out at their element bounds. Also reconciles the
    /// scroll for this frame, so the caller must prepaint at the returned
    /// bounds for index↔position math to agree with what is painted.
    fn single_line_text_bounds(
        &self,
        bounds: Bounds<Pixels>,
        layout_state: &mut InputLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Bounds<Pixels>> {
        let (focused, selection, whole_content_selected, previous_scroll) = {
            let input = self.input.read(cx);
            if input.mode != FieldMode::SingleLine {
                return None;
            }
            (
                input.is_visually_focused(window),
                (
                    input.selected_range.start,
                    input.cursor_offset(),
                    input.selected_range.end,
                ),
                !input.content.is_empty() && input.selected_range == (0..input.content.len()),
                input.scroll_offset,
            )
        };
        // Anchor the line at the natural origin first so index → position
        // can measure it; the definitive prepaint happens at the scrolled
        // origin returned from here.
        layout_state.text.prepaint(
            None,
            None,
            bounds,
            &mut layout_state.text_layout_state,
            window,
            cx,
        );
        let layout = layout_state.text.layout().clone();
        let x_for_index = |index: usize| {
            layout
                .position_for_index(index)
                .map_or(px(0.), |position| position.x - bounds.origin.x)
        };
        let text_width = x_for_index(layout.len());
        let scroll = if focused {
            let style = window.text_style();
            let font_id = window.text_system().resolve_font(&style.font());
            let font_size = style.font_size.to_pixels(window.rem_size());
            let em = window
                .text_system()
                .em_advance(font_id, font_size)
                .unwrap_or(px(8.));
            single_line_scroll(
                previous_scroll,
                bounds.size.width,
                em,
                text_width,
                (
                    x_for_index(selection.0),
                    x_for_index(selection.1),
                    x_for_index(selection.2),
                ),
                whole_content_selected,
            )
        } else {
            px(0.)
        };
        self.input
            .update(cx, |input, _| input.scroll_offset = scroll);
        Some(Bounds::new(
            point(bounds.origin.x - scroll, bounds.origin.y),
            size(bounds.size.width.max(text_width), bounds.size.height),
        ))
    }
}

struct InputLayoutState {
    text: StyledText,
    text_layout_state: (),
}

struct PrepaintState {
    cursor: Option<PaintQuad>,
}

/// Find-match washes layered into [`input_text_runs`]: every match gets
/// `match_color`, and the one navigation is on gets `active_color`, which also
/// wins over the selection wash so the current match keeps its identity while
/// selected — the way a find widget conventionally paints it.
struct SearchPaint<'a> {
    matches: &'a [Range<usize>],
    active: Option<&'a Range<usize>>,
    match_color: Hsla,
    active_color: Hsla,
}

impl SearchPaint<'static> {
    fn none() -> Self {
        Self {
            matches: &[],
            active: None,
            match_color: gpui::transparent_black(),
            active_color: gpui::transparent_black(),
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn input_text_runs(
    display_len: usize,
    base_run: TextRun,
    selected_range: Option<&Range<usize>>,
    marked_range: Option<&Range<usize>>,
    selection_color: Hsla,
    highlight: &[(Range<usize>, TokenClass)],
    token_color: impl Fn(TokenClass) -> Hsla,
    search: SearchPaint,
) -> Vec<TextRun> {
    let mut boundaries = vec![0, display_len];
    for range in [selected_range, marked_range].into_iter().flatten() {
        boundaries.push(range.start.min(display_len));
        boundaries.push(range.end.min(display_len));
    }
    for (range, _) in highlight {
        boundaries.push(range.start.min(display_len));
        boundaries.push(range.end.min(display_len));
    }
    for range in search.matches {
        boundaries.push(range.start.min(display_len));
        boundaries.push(range.end.min(display_len));
    }
    boundaries.sort_unstable();
    boundaries.dedup();

    // Token and match lists are sorted and non-overlapping, and every one of
    // their edges is a boundary, so each window has exactly one candidate —
    // found by binary search, keeping this linear-ish in the token count
    // rather than quadratic.
    let covering_match = |start: usize, end: usize| -> bool {
        let index = search.matches.partition_point(|range| range.end <= start);
        search
            .matches
            .get(index)
            .is_some_and(|range| range.start <= start && range.end >= end)
    };

    boundaries
        .windows(2)
        .filter_map(|boundary| {
            let start = boundary[0];
            let end = boundary[1];
            let token_index = highlight.partition_point(|(range, _)| range.end <= start);
            let color = highlight
                .get(token_index)
                .filter(|(range, _)| range.start <= start && range.end >= end)
                .map_or(base_run.color, |(_, class)| token_color(*class));
            let background_color = if search
                .active
                .is_some_and(|range| range.start <= start && range.end >= end)
            {
                Some(search.active_color)
            } else if selected_range.is_some_and(|range| range.start < end && range.end > start) {
                Some(selection_color)
            } else if covering_match(start, end) {
                Some(search.match_color)
            } else {
                None
            };
            (start < end).then(|| TextRun {
                len: end - start,
                color,
                background_color,
                underline: marked_range
                    .filter(|range| range.start < end && range.end > start)
                    .map(|_| UnderlineStyle {
                        color: Some(base_run.color),
                        thickness: px(1.0),
                        wavy: false,
                    }),
                ..base_run.clone()
            })
        })
        .collect()
}

impl IntoElement for InputElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for InputElement {
    type RequestLayoutState = InputLayoutState;
    type PrepaintState = PrepaintState;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let input = self.input.read(cx);
        let content = input.content.clone();
        let style = window.text_style();
        let theme = Theme::current(cx);
        let content_is_empty = content.is_empty();
        let (display_text, text_color, selected_range, marked_range) = if content_is_empty {
            (input.placeholder.clone(), theme.text_ghost, None, None)
        } else {
            (
                content,
                style.color,
                Some(&input.selected_range),
                input.marked_range.as_ref(),
            )
        };
        let base_run = TextRun {
            len: display_text.len(),
            font: style.font(),
            color: text_color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let palette = crate::md::render::Palette::from_theme(&theme);
        let search = if content_is_empty {
            SearchPaint::none()
        } else {
            SearchPaint {
                matches: &input.search_matches,
                active: input
                    .active_search_match
                    .and_then(|index| input.search_matches.get(index)),
                match_color: theme.warning.opacity(0.22),
                active_color: theme.warning.opacity(0.5),
            }
        };
        let runs = input_text_runs(
            display_text.len(),
            base_run,
            selected_range,
            marked_range,
            theme.inverse.opacity(0.18),
            if content_is_empty {
                &[]
            } else {
                &input.highlight
            },
            |class| palette.token(class),
            search,
        );
        let mut text = StyledText::new(display_text).with_runs(runs);
        let (layout_id, text_layout_state) = text.request_layout(id, inspector_id, window, cx);
        (
            layout_id,
            InputLayoutState {
                text,
                text_layout_state,
            },
        )
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        layout_state: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let text_bounds = self
            .single_line_text_bounds(bounds, layout_state, window, cx)
            .unwrap_or(bounds);
        layout_state.text.prepaint(
            None,
            None,
            text_bounds,
            &mut layout_state.text_layout_state,
            window,
            cx,
        );
        let theme = Theme::current(cx);
        let layout = layout_state.text.layout().clone();
        let (cursor_position, cursor, follow) = {
            let input = self.input.read(cx);
            let cursor = input.cursor_offset();
            let cursor_visible = cursor_should_be_visible(
                window.is_window_active(),
                input.focus_handle.is_focused(window),
                input.context_menu_preserves_visual_focus(),
                input.blink_cursor.read(cx).visible(),
            );
            // The caret's position feeds both its painted quad and the capped
            // viewport's follow below, so resolve it regardless of blink
            // phase or selection.
            let cursor_position = input
                .vertical_navigation
                .filter(|navigation| {
                    navigation.cursor_offset == cursor
                        && navigation.layout_width == layout.bounds().size.width
                })
                .map(|navigation| {
                    point(
                        layout.bounds().left() + navigation.cursor_x,
                        layout.bounds().top() + layout.line_height() * navigation.visual_row as f32,
                    )
                })
                .or_else(|| layout.position_for_index(cursor));
            let quad = (input.selected_range.is_empty() && cursor_visible)
                .then_some(cursor_position)
                .flatten()
                .map(|cursor_position| {
                    fill(
                        Bounds::new(cursor_position, size(px(1.5), layout.line_height())),
                        theme.accent,
                    )
                });
            let follow = input.auto_height.then(|| {
                (
                    (cursor, input.content.len(), layout.bounds().size.width),
                    input.caret_reconciled,
                    input.scroll_handle.clone(),
                )
            });
            (cursor_position, quad, follow)
        };
        if let Some((follow_state, reconciled, scroll_handle)) = follow
            && reconciled != Some(follow_state)
        {
            if let Some(position) = cursor_position {
                follow_caret(position, layout.line_height(), &scroll_handle, window);
            }
            self.input
                .update(cx, |input, _| input.caret_reconciled = Some(follow_state));
        }
        PrepaintState { cursor }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        layout_state: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let input = self.input.read(cx);
        let focus_handle = input.focus_handle.clone();
        let visually_focused = input.is_visually_focused(window);
        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, self.input.clone()),
            cx,
        );
        // Element-level mouse listeners are hitbox-gated, so a drag that
        // leaves the field would freeze the selection at the border. Track
        // the drag window-level instead — like a native text view, the
        // selection keeps extending outside the bounds because
        // index_for_mouse_position clamps an outside point to the nearest
        // line edge (above maps to the start, below to the end).
        window.on_mouse_event({
            let input = self.input.clone();
            move |event: &MouseMoveEvent, phase, window, cx| {
                if phase == DispatchPhase::Bubble && input.read(cx).is_selecting {
                    input.update(cx, |input, cx| input.on_mouse_move(event, window, cx));
                }
            }
        });
        layout_state.text.paint(
            None,
            None,
            bounds,
            &mut layout_state.text_layout_state,
            &mut (),
            window,
            cx,
        );
        if visually_focused && let Some(cursor) = prepaint.cursor.take() {
            window.paint_quad(cursor);
        }
        let text_layout = layout_state.text.layout().clone();
        self.input.update(cx, |input, _| {
            input.last_layout = Some(text_layout);
        });
    }
}

impl Render for TextInput {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::current(cx);
        let input = cx.entity();
        let context_menu_input = input.clone();
        let scroll_handle = self.scroll_handle.clone();
        let padding_x = self.padding_x;
        let scrollbar = self
            .auto_height
            .then(|| scrollbar::vertical(&self.scroll_handle, &self.scrollbar_state));
        let field = div()
            .key_context("TextInput")
            .id("composer-field")
            .track_focus(&self.focus_handle(cx))
            .cursor(CursorStyle::IBeam)
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::up))
            .on_action(cx.listener(Self::down))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::move_to_previous_word))
            .on_action(cx.listener(Self::move_to_next_word))
            .on_action(cx.listener(Self::select_to_start))
            .on_action(cx.listener(Self::select_to_end))
            .on_action(cx.listener(Self::select_to_previous_word))
            .on_action(cx.listener(Self::select_to_next_word))
            .on_action(cx.listener(Self::delete_to_start))
            .on_action(cx.listener(Self::delete_to_end))
            .on_action(cx.listener(Self::delete_to_previous_word))
            .on_action(cx.listener(Self::delete_to_next_word))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::undo))
            .on_action(cx.listener(Self::redo))
            .on_action(cx.listener(Self::enter))
            .on_action(cx.listener(Self::newline))
            .on_action(cx.listener(Self::submit_steer))
            .on_action(cx.listener(Self::clear_field))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::on_context_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .w_full()
            .text_color(theme.text)
            // An auto-height field owns its metrics; any other multi-line
            // field inherits the caller's, so a gutter beside an editor can
            // rely on the same line height.
            .when(self.auto_height, |field| {
                field
                    .min_h(px(24.0))
                    .max_h(AUTO_HEIGHT_MAX)
                    .overflow_y_scroll()
                    .track_scroll(&scroll_handle)
                    .px(padding_x)
                    .line_height(sp(22.0))
                    .text_size(sp(13.5))
            })
            // A single-line field never wraps: the overlong remainder slides
            // horizontally under this clipped viewport to follow the caret —
            // no scrollbar.
            .when(self.mode == FieldMode::SingleLine, |field| {
                field.whitespace_nowrap().overflow_hidden()
            })
            .child(InputElement { input });

        context_menu(
            div().w_full().child(field).children(scrollbar),
            "composer-context-menu",
            &self.context_menu,
            move |cx| {
                let (has_selection, has_content, all_selected) = {
                    let input = context_menu_input.read(cx);
                    let has_selection = !input.selected_range.is_empty();
                    let has_content = !input.content.is_empty();
                    let all_selected = has_content
                        && input.selected_range.start == 0
                        && input.selected_range.end == input.content.len();
                    (has_selection, has_content, all_selected)
                };
                let can_paste = cx
                    .read_from_clipboard()
                    .and_then(|item| item.text())
                    .is_some();

                // Call the editing methods directly rather than dispatching the
                // actions: by the time an item runs, focus is still unwinding
                // from the menu card, so a dispatch would have nowhere to land.
                let run = |input: &Entity<TextInput>,
                           action: fn(
                    &mut TextInput,
                    &mut Window,
                    &mut Context<TextInput>,
                )| {
                    let input = input.clone();
                    move |window: &mut Window, cx: &mut App| {
                        let focus = input.read(cx).focus_handle.clone();
                        window.focus(&focus, cx);
                        input.update(cx, |input, cx| action(input, window, cx));
                    }
                };

                vec![
                    MenuItem::new(
                        tr!("menu.cut"),
                        run(&context_menu_input, |input, window, cx| {
                            input.cut(&Cut, window, cx)
                        }),
                    )
                    .disabled(!has_selection),
                    MenuItem::new(
                        tr!("menu.copy"),
                        run(&context_menu_input, |input, window, cx| {
                            input.copy(&Copy, window, cx)
                        }),
                    )
                    .disabled(!has_selection),
                    MenuItem::new(
                        tr!("menu.paste"),
                        run(&context_menu_input, |input, window, cx| {
                            input.paste(&Paste, window, cx)
                        }),
                    )
                    .disabled(!can_paste),
                    MenuItem::Separator,
                    MenuItem::new(
                        tr!("menu.select_all"),
                        run(&context_menu_input, |input, window, cx| {
                            input.select_all(&SelectAll, window, cx)
                        }),
                    )
                    .disabled(!has_content || all_selected),
                ]
            },
        )
    }
}

impl Focusable for TextInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

/// What the composer tells its owner. `Submit` and `SubmitSteer` carry the
/// trimmed prompt; the remaining events report composer-level interactions
/// or mirror [`InputEvent`] from the embedded field.
#[derive(Clone)]
pub enum ComposerEvent {
    /// Enter: send the prompt, or queue it behind the running turn.
    Submit(String),
    /// Primary modifier + Enter: deliver the prompt into the running turn
    /// instead of queueing it behind the turn.
    SubmitSteer(String),
    /// Primary modifier + Enter in an empty composer: activate the oldest
    /// queued follow-up's Steer control.
    SteerQueued,
    Focus,
    Edited,
    /// Backspace in an already-empty composer — the chat idiom for "remove
    /// the last staged attachment".
    BackspaceOnEmpty,
}

/// An image or file paste, re-emitted from the embedded field's
/// [`MediaPaste`]. The owning view persists the entries and presents them as
/// attachment chips.
#[derive(Clone)]
pub struct ComposerAttachmentPaste(pub Vec<ClipboardEntry>);

/// The prompt composer, built on [`TextInput`]: a self-sizing multi-line
/// field where Enter submits the trimmed prompt and clears, the primary
/// modifier + Enter steers it into the running turn instead, and image or
/// file pastes surface as attachments rather than text. Everything textual —
/// editing, IME, undo, selection — is the embedded field's; this component
/// owns only the prompt policy on top.
pub struct ComposerInput {
    input: Entity<TextInput>,
    /// Clone of the embedded field's handle, so `focus()` needs no `cx`.
    focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl ComposerInput {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| {
            TextInput::new(window, cx)
                .multi_line()
                .submit_on_enter()
                .auto_height()
                .media_paste()
                .placeholder(tr!("input.do_anything"))
        });
        let focus_handle = input.read(cx).focus();
        let _subscriptions = vec![
            cx.subscribe(&input, |composer, _, event: &InputEvent, cx| match event {
                InputEvent::Submit(raw) => {
                    // The prompt is consumed by sending it, and whitespace
                    // alone is nothing to send.
                    let value = raw.trim().to_owned();
                    if !value.is_empty() {
                        composer.input.update(cx, |input, cx| input.clear(cx));
                        cx.emit(ComposerEvent::Submit(value));
                    }
                }
                InputEvent::Focus => cx.emit(ComposerEvent::Focus),
                InputEvent::Edited => cx.emit(ComposerEvent::Edited),
                InputEvent::BackspaceOnEmpty => cx.emit(ComposerEvent::BackspaceOnEmpty),
            }),
            cx.subscribe(&input, |_, _, event: &MediaPaste, cx| {
                cx.emit(ComposerAttachmentPaste(event.0.clone()));
            }),
        ];
        Self {
            input,
            focus_handle,
            _subscriptions,
        }
    }

    /// Forwarded [`TextInput::set_padding_x`], for the embedded field the
    /// constructor already created.
    pub fn padding_x(self, padding: Pixels, cx: &mut Context<Self>) -> Self {
        self.input
            .update(cx, |input, _| input.set_padding_x(padding));
        self
    }

    pub fn focus(&self) -> FocusHandle {
        self.focus_handle.clone()
    }

    pub fn content<'a>(&self, cx: &'a App) -> &'a str {
        self.input.read(cx).content()
    }

    /// Caret byte offset, for the autocomplete's trigger detection.
    pub fn cursor(&self, cx: &App) -> usize {
        self.input.read(cx).cursor()
    }

    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| input.clear(cx));
    }

    pub fn set_content(&mut self, content: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| input.set_content(content, cx));
    }

    pub fn set_placeholder(
        &mut self,
        placeholder: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) {
        self.input
            .update(cx, |input, cx| input.set_placeholder(placeholder, cx));
    }

    /// Splice `text` over `range`, as the `@`/`/` autocomplete accepts a row.
    pub fn replace_range(&mut self, range: Range<usize>, text: &str, cx: &mut Context<Self>) {
        self.input
            .update(cx, |input, cx| input.replace_range(range, text, cx));
    }

    pub fn preserve_visual_focus_for_context_menu(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.input.update(cx, |input, cx| {
            input.preserve_visual_focus_for_context_menu(window, cx)
        })
    }

    pub fn release_visual_focus_for_context_menu(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        self.input.update(cx, |input, cx| {
            input.release_visual_focus_for_context_menu(window, cx)
        });
    }

    /// Whether the embedded field's right-click menu is open.
    pub fn context_menu_open(&self, cx: &App) -> bool {
        self.input.read(cx).context_menu_open()
    }
}

impl Render for ComposerInput {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .w_full()
            // The embedded field propagates SubmitSteer; this ancestor
            // handler is where steering becomes a composer event.
            .on_action(cx.listener(|composer, _: &SubmitSteer, _, cx| {
                let value = composer.content(cx).trim().to_owned();
                if value.is_empty() {
                    cx.emit(ComposerEvent::SteerQueued);
                    return;
                }
                composer.input.update(cx, |input, cx| input.clear(cx));
                cx.emit(ComposerEvent::SubmitSteer(value));
            }))
            .child(self.input.clone())
    }
}

impl Focusable for ComposerInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<ComposerEvent> for ComposerInput {}
impl EventEmitter<ComposerAttachmentPaste> for ComposerInput {}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::path::PathBuf;
    use std::rc::Rc;
    use std::time::{Duration, Instant};

    use gpui::{
        ClipboardEntry, ClipboardItem, Context, Entity, EntityInputHandler, ExternalPaths, Image,
        ImageFormat, Pixels, Render, TestAppContext, TextRun, Window, div, font, hsla, prelude::*,
        px,
    };

    use super::TokenClass;
    use super::{
        ComposerEvent, ComposerInput, EditHistory, FieldMode, SearchPaint, TextInput,
        UNDO_GROUP_INTERVAL, UNDO_HISTORY_CAP, cursor_should_be_visible, input_text_runs,
        media_paste_entries, next_word_boundary, pasted_text_for_mode, previous_word_boundary,
        single_line_scroll, trimmed_splice, visual_row_count, word_range_at,
    };

    struct InputHarness {
        input: Entity<TextInput>,
        width: Pixels,
    }

    impl Render for InputHarness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().w(self.width).child(self.input.clone())
        }
    }

    struct ComposerHarness {
        composer: Entity<ComposerInput>,
    }

    impl Render for ComposerHarness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().w(px(300.)).child(self.composer.clone())
        }
    }

    fn setup_input<'a>(
        cx: &'a mut TestAppContext,
        content: &str,
        width: Pixels,
    ) -> (Entity<TextInput>, &'a mut gpui::VisualTestContext) {
        cx.update(super::init);
        let content = content.to_owned();
        let (harness, cx) = cx.add_window_view(move |window, cx| {
            let input = cx.new(|cx| {
                let mut input = TextInput::new(window, cx).multi_line().auto_height();
                input.set_content(content, cx);
                input
            });
            InputHarness { input, width }
        });
        let input = cx.read_entity(&harness, |harness, _| harness.input.clone());
        cx.update(|window, cx| window.focus(&input.read(cx).focus(), cx));
        cx.run_until_parked();
        (input, cx)
    }

    fn setup_composer<'a>(
        cx: &'a mut TestAppContext,
    ) -> (Entity<ComposerInput>, &'a mut gpui::VisualTestContext) {
        cx.update(super::init);
        let (harness, cx) = cx.add_window_view(|window, cx| {
            let composer = cx.new(|cx| ComposerInput::new(window, cx));
            ComposerHarness { composer }
        });
        let composer = cx.read_entity(&harness, |harness, _| harness.composer.clone());
        cx.update(|window, cx| window.focus(&composer.read(cx).focus(), cx));
        cx.run_until_parked();
        (composer, cx)
    }

    #[gpui::test]
    fn secondary_enter_steers_text_or_activates_the_queue(cx: &mut TestAppContext) {
        let (composer, cx) = setup_composer(cx);
        let events: Rc<RefCell<Vec<ComposerEvent>>> = Rc::default();
        let sink = events.clone();
        cx.update(|_, cx| {
            cx.subscribe(&composer, move |_, event: &ComposerEvent, _| {
                sink.borrow_mut().push(event.clone());
            })
            .detach();
        });

        cx.simulate_keystrokes("secondary-enter");
        assert!(matches!(
            events.borrow().last(),
            Some(ComposerEvent::SteerQueued)
        ));

        composer.update(cx, |composer, cx| composer.set_content("hold on", cx));
        events.borrow_mut().clear();
        cx.simulate_keystrokes("secondary-enter");
        assert!(
            events.borrow().iter().any(
                |event| matches!(event, ComposerEvent::SubmitSteer(text) if text == "hold on")
            )
        );
        cx.read_entity(&composer, |composer, cx| {
            assert_eq!(composer.content(cx), "")
        });
    }

    #[gpui::test]
    fn shift_enter_inserts_a_newline_at_the_caret(cx: &mut TestAppContext) {
        let (input, cx) = setup_input(cx, "hello world", px(300.));
        cx.update(|_, cx| input.update(cx, |input, cx| input.select_range(5..5, cx)));

        cx.simulate_keystrokes("shift-enter");

        cx.read_entity(&input, |input, _| {
            assert_eq!(input.content(), "hello\n world");
            assert_eq!(input.cursor(), 6);
        });
    }

    #[gpui::test]
    fn vertical_arrows_keep_the_original_column_across_short_lines(cx: &mut TestAppContext) {
        let (input, cx) = setup_input(cx, "abcdef\nx\nabcdef", px(300.));
        cx.update(|_, cx| input.update(cx, |input, cx| input.select_range(5..5, cx)));

        cx.simulate_keystrokes("down");
        cx.read_entity(&input, |input, _| assert_eq!(input.cursor(), 8));

        cx.simulate_keystrokes("down");
        cx.read_entity(&input, |input, _| assert_eq!(input.cursor(), 14));

        cx.simulate_keystrokes("up");
        cx.read_entity(&input, |input, _| assert_eq!(input.cursor(), 8));
    }

    #[gpui::test]
    fn vertical_arrows_advance_across_soft_wrap_boundaries(cx: &mut TestAppContext) {
        let text = "one two three four five six seven eight nine ten eleven twelve";
        let (input, cx) = setup_input(cx, text, px(90.));
        cx.read_entity(&input, |input, _| {
            assert!(
                visual_row_count(input.last_layout.as_ref().unwrap()) >= 3,
                "fixture must wrap across at least three visual rows"
            );
        });
        cx.update(|_, cx| input.update(cx, |input, cx| input.select_range(0..0, cx)));

        cx.simulate_keystrokes("down");
        let first_boundary = cx.read_entity(&input, |input, _| input.cursor());
        assert!(first_boundary > 0);

        cx.simulate_keystrokes("down");
        cx.read_entity(&input, |input, _| {
            assert!(
                input.cursor() > first_boundary,
                "the wrap-boundary affinity must not leave the caret stuck"
            );
        });
    }

    #[test]
    fn image_and_file_first_clipboards_become_attachments() {
        let image = Image {
            format: ImageFormat::Png,
            bytes: vec![1, 2, 3],
            id: 7,
        };
        let image_clipboard = ClipboardItem::new_image(&image);
        assert!(matches!(
            media_paste_entries(&image_clipboard).as_deref(),
            Some([ClipboardEntry::Image(found)]) if found == &image
        ));

        let paths = ExternalPaths(vec![PathBuf::from("/tmp/reference.png")].into());
        let file_clipboard = ClipboardItem {
            entries: vec![
                ClipboardEntry::ExternalPaths(paths.clone()),
                ClipboardEntry::from("/tmp/reference.png".to_owned()),
            ],
        };
        assert!(matches!(
            media_paste_entries(&file_clipboard).as_deref(),
            Some([ClipboardEntry::ExternalPaths(found)]) if found == &paths
        ));
    }

    #[test]
    fn text_first_clipboard_keeps_text_paste_priority() {
        let clipboard = ClipboardItem {
            entries: vec![
                ClipboardEntry::from("keep this text".to_owned()),
                ClipboardEntry::Image(Image {
                    format: ImageFormat::Png,
                    bytes: vec![1, 2, 3],
                    id: 8,
                }),
            ],
        };
        assert!(media_paste_entries(&clipboard).is_none());
    }

    #[test]
    fn multi_line_paste_keeps_line_breaks() {
        assert_eq!(
            pasted_text_for_mode(FieldMode::MultiLine, "first\r\nsecond\rthird\nfourth"),
            "first\nsecond\nthird\nfourth"
        );
        // A one-line field is still one line.
        assert_eq!(
            pasted_text_for_mode(FieldMode::SingleLine, "first\r\nsecond"),
            "first  second"
        );
    }

    #[gpui::test]
    fn escape_clears_an_opted_in_field_first(cx: &mut TestAppContext) {
        cx.update(super::init);
        let (harness, cx) = cx.add_window_view(|window, cx| {
            let input = cx.new(|cx| {
                let mut input = TextInput::new(window, cx).clear_on_escape();
                input.set_content("abc", cx);
                input
            });
            InputHarness {
                input,
                width: px(300.),
            }
        });
        let input = cx.read_entity(&harness, |harness, _| harness.input.clone());
        cx.update(|window, cx| window.focus(&input.read(cx).focus(), cx));
        cx.run_until_parked();

        cx.simulate_keystrokes("escape");

        cx.read_entity(&input, |input, _| assert_eq!(input.content(), ""));
    }

    #[gpui::test]
    fn escape_leaves_a_field_that_did_not_opt_in(cx: &mut TestAppContext) {
        let (input, cx) = setup_input(cx, "abc", px(300.));

        cx.simulate_keystrokes("escape");

        cx.read_entity(&input, |input, _| assert_eq!(input.content(), "abc"));
    }

    /// Type each string in sequence at `at`, advancing the caret, the way
    /// keystrokes arrive: record first, then apply the splice.
    fn type_at(
        history: &mut EditHistory,
        content: &mut String,
        mut at: usize,
        keys: &[&str],
        start: Instant,
        gap: Duration,
    ) -> usize {
        for (index, key) in keys.iter().enumerate() {
            history.record(
                content,
                &(at..at),
                key,
                at..at,
                false,
                start + gap * index as u32,
            );
            content.insert_str(at, key);
            at += key.len();
        }
        at
    }

    #[test]
    fn a_typing_run_coalesces_into_one_undo_step() {
        let mut history = EditHistory::default();
        let mut content = String::from("fn main() {}");
        let start = Instant::now();
        type_at(
            &mut history,
            &mut content,
            3,
            &["a", "b", "c"],
            start,
            Duration::from_millis(50),
        );
        assert_eq!(content, "fn abcmain() {}");

        let (restored, selection, _) = history.undo(&content).unwrap();
        assert_eq!(restored, "fn main() {}");
        assert_eq!(selection, 3..3, "caret returns to where typing began");
        assert!(
            history.undo(&restored).is_none(),
            "the whole run is one step"
        );

        let (redone, selection, _) = history.redo(&restored).unwrap();
        assert_eq!(redone, "fn abcmain() {}");
        assert_eq!(selection, 6..6, "caret lands after the reapplied text");
    }

    #[test]
    fn a_pause_starts_a_new_undo_step() {
        let mut history = EditHistory::default();
        let mut content = String::new();
        let start = Instant::now();
        type_at(&mut history, &mut content, 0, &["a"], start, Duration::ZERO);
        type_at(
            &mut history,
            &mut content,
            1,
            &["b"],
            start + UNDO_GROUP_INTERVAL,
            Duration::ZERO,
        );
        assert_eq!(history.undo.len(), 2, "the group interval is exclusive");
    }

    #[test]
    fn a_backspace_run_undoes_as_one_step() {
        let mut history = EditHistory::default();
        let mut content = String::from("hello");
        let start = Instant::now();
        // Backspace twice: each records the extended selection it deletes.
        history.record(&content, &(4..5), "", 4..5, false, start);
        content.replace_range(4..5, "");
        history.record(
            &content,
            &(3..4),
            "",
            3..4,
            false,
            start + Duration::from_millis(50),
        );
        content.replace_range(3..4, "");
        assert_eq!(content, "hel");

        let (restored, selection, _) = history.undo(&content).unwrap();
        assert_eq!(restored, "hello");
        assert_eq!(selection, 4..5, "the first deletion's selection returns");
        assert!(history.undo(&restored).is_none());
    }

    #[test]
    fn typing_extends_a_replaced_selection_into_one_step() {
        let mut history = EditHistory::default();
        let mut content = String::from("abcdef");
        let start = Instant::now();
        // Type "x" over the selected "cde", then keep typing.
        history.record(&content, &(2..5), "x", 2..5, false, start);
        content.replace_range(2..5, "x");
        type_at(
            &mut history,
            &mut content,
            3,
            &["y"],
            start + Duration::from_millis(50),
            Duration::ZERO,
        );
        assert_eq!(content, "abxyf");

        let (restored, selection, _) = history.undo(&content).unwrap();
        assert_eq!(restored, "abcdef");
        assert_eq!(selection, 2..5, "the replaced selection comes back");
        assert!(history.undo(&restored).is_none());
    }

    #[test]
    fn sealed_steps_do_not_coalesce() {
        let mut history = EditHistory::default();
        let mut content = String::new();
        let start = Instant::now();
        type_at(&mut history, &mut content, 0, &["a"], start, Duration::ZERO);
        history.seal();
        type_at(
            &mut history,
            &mut content,
            1,
            &["b"],
            start + Duration::from_millis(10),
            Duration::ZERO,
        );
        assert_eq!(history.undo.len(), 2);
    }

    #[test]
    fn an_edit_after_undo_drops_the_redo_branch() {
        let mut history = EditHistory::default();
        let mut content = String::new();
        let start = Instant::now();
        type_at(&mut history, &mut content, 0, &["a"], start, Duration::ZERO);
        let (restored, ..) = history.undo(&content).unwrap();
        let mut content = restored;
        assert_eq!(history.redo.len(), 1);
        type_at(
            &mut history,
            &mut content,
            0,
            &["b"],
            start + UNDO_GROUP_INTERVAL * 2,
            Duration::ZERO,
        );
        assert!(history.redo.is_empty());
        assert!(history.redo(&content).is_none());
    }

    /// Replace All arrives as one whole-content splice; the record must keep
    /// only the span that changed, not two copies of the file.
    #[test]
    fn a_whole_content_replace_records_only_the_changed_span() {
        let mut history = EditHistory::default();
        let content = "xx aaa yy";
        let replaced = "xx bbb yy";
        history.record(
            content,
            &(0..content.len()),
            replaced,
            0..content.len(),
            false,
            Instant::now(),
        );
        let record = history.undo.last().unwrap();
        assert_eq!((record.start, record.old.as_str()), (3, "aaa"));
        assert_eq!(record.new, "bbb");

        let (restored, ..) = history.undo(replaced).unwrap();
        assert_eq!(restored, content);
    }

    #[test]
    fn trimmed_splices_respect_character_boundaries() {
        // "é" and "è" share their first UTF-8 byte; the shared byte must not
        // be trimmed off mid-character.
        assert_eq!(
            trimmed_splice("é", &(0..2), "è"),
            (0, "é".to_owned(), "è".to_owned())
        );
        assert_eq!(
            trimmed_splice("aé", &(0..3), "bé"),
            (0, "a".to_owned(), "b".to_owned())
        );
        assert_eq!(
            trimmed_splice("abc", &(0..3), "abcd"),
            (3, String::new(), "d".to_owned())
        );
    }

    /// Every marked-text revision and the final commit collapse into a
    /// single undo step, the way Zed groups everything since the first IME
    /// edit into one transaction.
    #[test]
    fn a_composition_undoes_as_one_step() {
        let mut history = EditHistory::default();
        let start = Instant::now();
        let mut content = String::from("ab");
        history.record_composition(&content, &(1..1), "k", 1..1, false, start);
        content.replace_range(1..1, "k");
        history.record_composition(&content, &(1..2), "か", 1..2, false, start);
        content.replace_range(1..2, "か");
        history.record_composition(&content, &(1..4), "漢字", 1..4, false, start);
        content.replace_range(1..4, "漢字");
        history.finalize_composition();
        assert_eq!(content, "a漢字b");
        assert_eq!(history.undo.len(), 1);

        let (restored, selection, _) = history.undo(&content).unwrap();
        assert_eq!(restored, "ab");
        assert_eq!(selection, 1..1);

        let (redone, ..) = history.redo(&restored).unwrap();
        assert_eq!(redone, "a漢字b");
    }

    #[test]
    fn a_canceled_composition_records_nothing() {
        let mut history = EditHistory::default();
        let start = Instant::now();
        let mut content = String::from("ab");
        history.record_composition(&content, &(1..1), "k", 1..1, false, start);
        content.replace_range(1..1, "k");
        history.record_composition(&content, &(1..2), "", 1..2, false, start);
        content.replace_range(1..2, "");
        history.finalize_composition();
        assert_eq!(content, "ab");
        assert!(history.undo.is_empty());
    }

    /// A step that no longer describes the content — the invariant only a
    /// bug could break — must drop the history rather than corrupt the text.
    #[test]
    fn a_stale_step_is_dropped_rather_than_applied() {
        let mut history = EditHistory::default();
        let mut content = String::new();
        type_at(
            &mut history,
            &mut content,
            0,
            &["abc"],
            Instant::now(),
            Duration::ZERO,
        );
        assert!(history.undo("unrelated content").is_none());
        assert!(history.undo.is_empty() && history.redo.is_empty());
    }

    #[test]
    fn history_is_capped() {
        let mut history = EditHistory::default();
        let mut content = String::new();
        let start = Instant::now();
        for step in 0..UNDO_HISTORY_CAP + 5 {
            let end = content.len();
            let at = type_at(
                &mut history,
                &mut content,
                end,
                &["x"],
                start + UNDO_GROUP_INTERVAL * step as u32,
                Duration::ZERO,
            );
            assert_eq!(at, content.len());
        }
        assert_eq!(history.undo.len(), UNDO_HISTORY_CAP);
    }

    #[test]
    fn word_navigation_matches_native_text_inputs() {
        let text = "hello, world  👋";

        assert_eq!(next_word_boundary(text, 0), 5);
        assert_eq!(next_word_boundary(text, 5), 6);
        assert_eq!(next_word_boundary(text, 6), 12);
        assert_eq!(next_word_boundary(text, 12), text.len());

        assert_eq!(previous_word_boundary(text, text.len()), 14);
        assert_eq!(previous_word_boundary(text, 14), 7);
        assert_eq!(previous_word_boundary(text, 7), 5);
        assert_eq!(previous_word_boundary(text, 5), 0);
    }

    #[test]
    fn double_click_ranges_follow_unicode_word_boundaries() {
        let text = "hello,  world 👋";

        assert_eq!(word_range_at(text, 1), 0..5);
        assert_eq!(word_range_at(text, 5), 5..6);
        assert_eq!(word_range_at(text, 6), 6..8);
        assert_eq!(word_range_at(text, 9), 8..13);
        assert_eq!(word_range_at(text, 14), 14..text.len());
        assert_eq!(word_range_at(text, text.len()), text.len()..text.len());
    }

    /// ASCII keeps UTF-16 and UTF-8 offsets in step, so composing after it has
    /// to land exactly where it always did.
    #[gpui::test]
    fn ime_composition_after_ascii_keeps_its_caret(cx: &mut TestAppContext) {
        let (input, cx) = setup_input(cx, "hi", px(300.));
        cx.update(|_, cx| input.update(cx, |input, cx| input.select_range(2..2, cx)));

        cx.update(|window, cx| {
            input.update(cx, |input, cx| {
                input.replace_and_mark_text_in_range(None, "k", Some(1..1), window, cx);
            })
        });

        cx.read_entity(&input, |input, _| {
            assert_eq!(input.content(), "hik");
            assert_eq!(input.marked_range, Some(2..3));
            assert_eq!(input.selected_range, 3..3);
        });
    }

    /// Multi-byte content ahead of the composition makes UTF-16 and UTF-8
    /// offsets diverge, which used to place the caret past the end of the
    /// content and abort the next slice.
    #[gpui::test]
    fn ime_selection_stays_inside_content_after_multibyte_text(cx: &mut TestAppContext) {
        let (input, cx) = setup_input(cx, "あ", px(300.));
        cx.update(|_, cx| input.update(cx, |input, cx| input.select_range(3..3, cx)));

        cx.update(|window, cx| {
            input.update(cx, |input, cx| {
                input.replace_and_mark_text_in_range(None, "k", Some(1..1), window, cx);
            })
        });

        cx.read_entity(&input, |input, _| {
            assert_eq!(input.content(), "あk");
            assert_eq!(input.marked_range, Some(3..4));
            assert_eq!(input.selected_range, 4..4);
        });
    }

    #[test]
    fn context_menu_keeps_cursor_visible_while_it_owns_focus() {
        assert!(cursor_should_be_visible(true, false, true, false));
        assert!(!cursor_should_be_visible(true, false, false, true));
        assert!(!cursor_should_be_visible(false, false, true, true));
    }

    /// Syntax colours and the selection wash are independent layers over the
    /// same text, so their boundaries have to interleave without either losing
    /// coverage — the runs must still tile the content exactly.
    #[test]
    fn syntax_colours_and_selection_split_into_tiling_runs() {
        let selection = 4..12;
        let keyword = hsla(0.8, 0.5, 0.6, 1.0);
        let plain = hsla(0.0, 0.0, 1.0, 1.0);
        // "let" at 0..3 and "true" at 8..12, with a selection cutting across.
        let highlight = vec![(0..3, TokenClass::Keyword), (8..12, TokenClass::Literal)];
        let runs = input_text_runs(
            12,
            TextRun {
                len: 12,
                font: font(".SystemUIFont"),
                color: plain,
                background_color: None,
                underline: None,
                strikethrough: None,
            },
            Some(&selection),
            None,
            hsla(0.0, 0.0, 1.0, 0.18),
            &highlight,
            |class| match class {
                TokenClass::Keyword => keyword,
                _ => plain,
            },
            SearchPaint::none(),
        );

        assert_eq!(
            runs.iter().map(|run| run.len).sum::<usize>(),
            12,
            "runs must tile the content: {runs:?}"
        );
        // The keyword keeps its colour and stays outside the selection.
        assert_eq!(runs[0].len, 3);
        assert_eq!(runs[0].color, keyword);
        assert!(runs[0].background_color.is_none());
        // Everything inside the selection carries the wash.
        let selected_len: usize = runs
            .iter()
            .filter(|run| run.background_color.is_some())
            .map(|run| run.len)
            .sum();
        assert_eq!(selected_len, selection.len());
    }

    #[test]
    fn selection_and_ime_styles_survive_wrapped_text_run_splitting() {
        let selection = 2..8;
        let marked = 4..6;
        let runs = input_text_runs(
            10,
            TextRun {
                len: 10,
                font: font(".SystemUIFont"),
                color: hsla(0.0, 0.0, 1.0, 1.0),
                background_color: None,
                underline: None,
                strikethrough: None,
            },
            Some(&selection),
            Some(&marked),
            hsla(0.0, 0.0, 1.0, 0.18),
            &[],
            |_| hsla(0.0, 0.0, 1.0, 1.0),
            SearchPaint::none(),
        );

        assert_eq!(
            runs.iter().map(|run| run.len).collect::<Vec<_>>(),
            [2, 2, 2, 2, 2]
        );
        assert_eq!(
            runs.iter()
                .map(|run| run.background_color.is_some())
                .collect::<Vec<_>>(),
            [false, true, true, true, false]
        );
        assert_eq!(
            runs.iter()
                .map(|run| run.underline.is_some())
                .collect::<Vec<_>>(),
            [false, false, true, false, false]
        );
    }

    /// Find washes tile with everything else, and the active match keeps its
    /// own colour even while it is also the selection — otherwise navigating
    /// (which selects the match) would make the current match look like any
    /// drag-selection.
    #[test]
    fn search_match_washes_layer_under_selection_except_the_active_one() {
        let plain = hsla(0.0, 0.0, 1.0, 1.0);
        let selection_color = hsla(0.6, 1.0, 0.5, 0.4);
        let match_color = hsla(0.1, 1.0, 0.5, 0.2);
        let active_color = hsla(0.1, 1.0, 0.5, 0.5);
        let matches = vec![2..4, 8..10, 14..16];
        let active = 8..10;
        // Selection covers the active match exactly, as after find-next.
        let selection = 8..10;
        let runs = input_text_runs(
            20,
            TextRun {
                len: 20,
                font: font(".SystemUIFont"),
                color: plain,
                background_color: None,
                underline: None,
                strikethrough: None,
            },
            Some(&selection),
            None,
            selection_color,
            &[],
            |_| plain,
            SearchPaint {
                matches: &matches,
                active: Some(&active),
                match_color,
                active_color,
            },
        );

        assert_eq!(runs.iter().map(|run| run.len).sum::<usize>(), 20);
        let background_at = |offset: usize| {
            let mut cursor = 0;
            runs.iter()
                .find_map(|run| {
                    let range = cursor..cursor + run.len;
                    cursor += run.len;
                    range.contains(&offset).then_some(run.background_color)
                })
                .unwrap()
        };
        assert_eq!(background_at(2), Some(match_color));
        assert_eq!(background_at(8), Some(active_color));
        assert_eq!(background_at(14), Some(match_color));
        assert_eq!(background_at(6), None);
    }

    /// The single-line scroll follows the caret with an em of lookahead and
    /// otherwise moves as little as possible.
    #[test]
    fn single_line_scroll_follows_the_caret() {
        use gpui::px;
        let caret = |x: f32| (px(x), px(x), px(x));

        // Text narrower than the viewport never scrolls, whatever is stale.
        assert_eq!(
            single_line_scroll(px(40.), px(200.), px(8.), px(100.), caret(100.), false),
            px(0.)
        );
        // Caret at the end of long text: the end comes into view with the
        // lookahead, which is exactly the maximum scroll.
        assert_eq!(
            single_line_scroll(px(0.), px(200.), px(8.), px(1000.), caret(1000.), false),
            px(808.)
        );
        // Caret already visible: the view holds still.
        assert_eq!(
            single_line_scroll(px(300.), px(200.), px(8.), px(1000.), caret(400.), false),
            px(300.)
        );
        // Caret left of the view: align it at the left edge.
        assert_eq!(
            single_line_scroll(px(300.), px(200.), px(8.), px(1000.), caret(250.), false),
            px(250.)
        );
        // Stale scroll past the end of shrunk text clamps back into range.
        assert_eq!(
            single_line_scroll(px(900.), px(200.), px(8.), px(400.), caret(0.), false),
            px(0.)
        );
    }

    /// Selections reveal their whole span when it fits; a wider partial
    /// selection follows its head, and select-all holds the view still.
    #[test]
    fn single_line_scroll_reveals_selections_by_zed_rules() {
        use gpui::px;

        // A selection that fits scrolls just enough to show all of it.
        assert_eq!(
            single_line_scroll(
                px(0.),
                px(200.),
                px(8.),
                px(1000.),
                (px(300.), px(400.), px(400.)),
                false
            ),
            px(208.)
        );
        // A partial selection wider than the viewport tracks its head.
        assert_eq!(
            single_line_scroll(
                px(0.),
                px(200.),
                px(8.),
                px(1000.),
                (px(100.), px(600.), px(600.)),
                false
            ),
            px(408.)
        );
        // …including a reversed head extending left.
        assert_eq!(
            single_line_scroll(
                px(400.),
                px(200.),
                px(8.),
                px(1000.),
                (px(100.), px(100.), px(600.)),
                false
            ),
            px(100.)
        );
        // Select-all keeps whatever was shown, only clamped into range.
        assert_eq!(
            single_line_scroll(
                px(300.),
                px(200.),
                px(8.),
                px(1000.),
                (px(0.), px(1000.), px(1000.)),
                true
            ),
            px(300.)
        );
    }
}

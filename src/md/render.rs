//! [`BlockTree`] → GPUI elements.
//!
//! Two properties drive every decision here.
//!
//! **One shaped element per block.** A paragraph becomes a single
//! [`StyledText`] over one flat string with `TextRun`s for its inline styles —
//! not one element per line, and not an element per styled span. Layout cost
//! per block is therefore one measured-layout node and one `shape_text` call,
//! which GPUI's line-layout cache reuses verbatim across frames when the text
//! and wrap width are unchanged.
//!
//! **Color is paint, geometry is layout.** Syntax highlighting, inline-code
//! washes and the selection wash are all painted from geometry read back out of
//! the text's own [`TextLayout`], so none of them can change a row's measured
//! height. That is what lets a streaming code block colorize progressively
//! without ever reflowing, and what keeps the transcript's row measurements
//! stable while a selection is dragged across it.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, BorderStyle, Bounds, ClipboardItem, CursorStyle, DispatchPhase, Font, FontStyle,
    FontWeight, Hsla, InteractiveText, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, ParentElement, Pixels, Point, SharedString, StrikethroughStyle,
    StyledText, TextLayout, TextRun, UnderlineStyle, Window, canvas, div, font, img, point,
    prelude::*, px, quad, relative, size,
};

use super::highlight::{self, Lang, TokenClass};
use super::mend::PENDING_LINK_URL;
use super::parser::{Block, IncrementalParser, InlineRun, ListItem, TableAlign, TopBlock};
use super::selection::{
    RegisteredText, SelectionRegistry, SelectionState, TextKey, line_range, word_range,
};
use super::veil::{RowVeil, apply_veil};
use crate::theme::Theme;
use crate::ui::tooltip::Tooltip;

/// Selection geometry: the laid-out text handle for one painted element.
pub type TextGeometry = TextLayout;

/// The transcript's shared selection handles, specialised to real geometry.
pub type TranscriptSelection = SelectionState<TextGeometry>;

/// An optional app-owned override for clicked markdown links.
///
/// The markdown renderer stays unaware of projects and workspace surfaces;
/// callers that do have that context can intercept a link, while every other
/// markdown view continues to use GPUI's ordinary URL opener.
pub type LinkHandler = Rc<dyn Fn(&str, &mut Window, &mut gpui::App)>;

// ── Layout metrics ─────────────────────────────────────────────────────────
//
// Everything in this block participates in measurement, so these are the only
// numbers that can change a transcript row's height.

/// Paragraph and inline metrics for one text scale.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Metrics {
    pub text_size: f32,
    pub line_height: f32,
    pub code_text_size: f32,
    pub code_line_height: f32,
    /// Vertical gap between sibling blocks.
    pub block_gap: f32,
}

impl Metrics {
    /// Assistant response scale, matching the transcript's body text.
    pub const BODY: Self = Self {
        text_size: 13.5,
        line_height: 21.0,
        code_text_size: 11.5,
        code_line_height: 17.5,
        block_gap: 10.0,
    };

    /// User-message scale. Markdown blocks keep the bubble's established body
    /// geometry instead of making every existing plain prompt subtly reflow.
    pub const USER_MESSAGE: Self = Self {
        text_size: 14.0,
        line_height: 20.0,
        code_text_size: 11.5,
        code_line_height: 17.5,
        block_gap: 10.0,
    };

    /// Compact scale for tool output and other secondary detail.
    pub const COMPACT: Self = Self {
        text_size: 11.5,
        line_height: 17.0,
        code_text_size: 10.5,
        code_line_height: 16.0,
        block_gap: 6.0,
    };
}

pub const SANS_FAMILY: &str = ".SystemUIFont";
/// The bundled mono face. "SF Mono" only exists on machines that installed it
/// with Xcode or Terminal, and silently falls back to the sans face when it
/// does not — which reads as proportional code.
pub const MONO_FAMILY: &str = "JetBrains Mono";

/// Inline-code wash geometry. Paint-only: the box overhangs the glyphs
/// horizontally and insets vertically inside the line box.
const CODE_WASH_RADIUS: f32 = 4.0;
const CODE_WASH_PAD_X: f32 = 2.5;
const CODE_WASH_INSET_Y: f32 = 1.5;

/// Heading scale relative to body text, by level.
fn heading_metrics(level: u8, metrics: &Metrics) -> (f32, f32, FontWeight) {
    let (scale, weight) = match level {
        1 => (1.45, FontWeight::BOLD),
        2 => (1.28, FontWeight::BOLD),
        3 => (1.14, FontWeight::SEMIBOLD),
        4 => (1.05, FontWeight::SEMIBOLD),
        _ => (1.0, FontWeight::SEMIBOLD),
    };
    let size = (metrics.text_size * scale).round();
    (size, (size * 1.42).round(), weight)
}

// ── Palette ────────────────────────────────────────────────────────────────

/// Colors for markdown paint, resolved once per render from the theme.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Palette {
    pub text: Hsla,
    pub secondary: Hsla,
    pub tertiary: Hsla,
    pub ghost: Hsla,
    pub border: Hsla,
    pub inset: Hsla,
    pub overlay: Hsla,
    pub code_text: Hsla,
    pub code_wash: Hsla,
    pub selection: Hsla,
    pub accent: Hsla,
    pub added: Hsla,
    pub removed: Hsla,
    is_dark: bool,
}

impl Palette {
    pub fn from_theme(theme: &Theme) -> Self {
        Self {
            text: theme.text,
            secondary: theme.text_secondary,
            tertiary: theme.text_tertiary,
            ghost: theme.text_ghost,
            border: theme.border,
            inset: theme.inset,
            overlay: theme.overlay,
            code_text: theme.code_text,
            code_wash: theme.code_wash,
            selection: theme.selection,
            accent: theme.accent,
            added: theme.success,
            removed: theme.danger,
            is_dark: theme.is_dark,
        }
    }

    /// Token colors. Deliberately restrained: three hues plus muted comments,
    /// so a code block still reads as part of a graphite transcript. Shared with
    /// the code editor, so both surfaces colour code identically.
    pub fn token(&self, class: TokenClass) -> Hsla {
        let dark = self.is_dark;
        match class {
            TokenClass::Keyword => hue(dark, 0xC98BC0, 0x9A4B92),
            TokenClass::Literal => hue(dark, 0xD9A05B, 0x9A6019),
            TokenClass::String => hue(dark, 0x94C08A, 0x3F7A36),
            TokenClass::Comment => self.ghost,
            TokenClass::Number => hue(dark, 0xD9A05B, 0x9A6019),
            TokenClass::Type => hue(dark, 0x8FB8D9, 0x2F6690),
            TokenClass::Function => hue(dark, 0x8FB8D9, 0x2F6690),
            TokenClass::Meta => self.tertiary,
            TokenClass::Added => self.added,
            TokenClass::Removed => self.removed,
        }
    }
}

fn hue(is_dark: bool, dark: u32, light: u32) -> Hsla {
    gpui::rgb(if is_dark { dark } else { light }).into()
}

// ── Flattened inline text ──────────────────────────────────────────────────

/// One block's inline content, ready to shape: a flat string, the `TextRun`s
/// that tile it exactly, plus the byte ranges that need paint-only decoration.
#[derive(Debug)]
pub struct FlatText {
    pub text: SharedString,
    pub runs: Vec<TextRun>,
    pub links: Vec<(Range<usize>, String)>,
    pub code_ranges: Vec<Range<usize>>,
}

/// Flatten inline runs for shaping. Pure given the palette and base weight.
pub fn flatten(
    runs: &[InlineRun],
    palette: &Palette,
    base_weight: FontWeight,
    base_color: Hsla,
) -> FlatText {
    let mut text = String::new();
    let mut out: Vec<TextRun> = Vec::with_capacity(runs.len());
    let mut links: Vec<(Range<usize>, String)> = Vec::new();
    let mut code_ranges: Vec<Range<usize>> = Vec::new();

    for run in runs {
        if run.text.is_empty() {
            continue;
        }
        let start = text.len();
        text.push_str(&run.text);
        let end = text.len();

        let mut run_font = font(if run.style.code {
            MONO_FAMILY
        } else {
            SANS_FAMILY
        });
        run_font.weight = if run.style.bold && base_weight < FontWeight::SEMIBOLD {
            FontWeight::SEMIBOLD
        } else {
            base_weight
        };
        run_font.style = if run.style.italic {
            FontStyle::Italic
        } else {
            FontStyle::Normal
        };

        if run.style.code {
            // Merge neighbouring code runs so their washes form one box.
            match code_ranges.last_mut() {
                Some(range) if range.end == start => range.end = end,
                _ => code_ranges.push(start..end),
            }
        }
        if let Some(url) = &run.style.link {
            // A still-streaming link keeps link styling — so the URL settling
            // changes nothing visually — but must not become clickable.
            if url != PENDING_LINK_URL {
                match links.last_mut() {
                    Some((range, last)) if range.end == start && last == url => range.end = end,
                    _ => links.push((start..end, url.clone())),
                }
            }
        }

        out.push(TextRun {
            len: run.text.len(),
            font: run_font,
            color: if run.style.code {
                palette.code_text
            } else {
                base_color
            },
            // Inline code's wash is painted as *rounded* quads by the canvas
            // underlay; a run background could only ever be a square box.
            background_color: None,
            underline: run.style.link.is_some().then_some(UnderlineStyle {
                color: Some(palette.tertiary),
                thickness: px(1.0),
                wavy: false,
            }),
            strikethrough: run.style.strikethrough.then_some(StrikethroughStyle {
                thickness: px(1.0),
                color: Some(palette.tertiary),
            }),
        });
    }

    FlatText {
        text: text.into(),
        runs: out,
        links,
        code_ranges,
    }
}

/// A flat string with uniform styling, for non-markdown transcript text.
pub fn flatten_plain(
    text: impl Into<SharedString>,
    family: &'static str,
    weight: FontWeight,
    color: Hsla,
) -> FlatText {
    let text: SharedString = text.into();
    let mut run_font = font(family);
    run_font.weight = weight;
    let runs = if text.is_empty() {
        Vec::new()
    } else {
        vec![TextRun {
            len: text.len(),
            font: run_font,
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
        }]
    };
    FlatText {
        text,
        runs,
        links: Vec::new(),
        code_ranges: Vec::new(),
    }
}

// ── Per-message state ──────────────────────────────────────────────────────

/// Everything the renderer keeps between frames for one markdown body.
///
/// The flatten cache is keyed by element ordinal and pruned only back to the
/// parser's stable prefix, so a streamed delta rebuilds the final block's
/// elements and reuses every settled one.
pub struct MarkdownView {
    parser: IncrementalParser,
    /// Mended replacement for the final block while streaming.
    tail: Vec<TopBlock>,
    flats: RefCell<HashMap<usize, Rc<FlatText>>>,
    /// First element ordinal belonging to the final block — the only block an
    /// append can change. Recorded during render, because only the renderer
    /// knows how many text elements each block expands into.
    volatile_from: Cell<usize>,
    /// Style the cached flats were built for. Colors live inside `TextRun`s, so
    /// a theme switch has to drop them or the transcript keeps painting the old
    /// palette.
    style: Cell<Option<(Palette, Metrics)>>,
    /// Per-element opacity spans for the live response. Text is committed to
    /// layout immediately; only these paint colors animate.
    veil: RefCell<RowVeil>,
    /// Code-block ordinals currently showing successful copy feedback. Kept
    /// outside the parsed/flattened caches so a three-second icon change never
    /// invalidates text shaping.
    copied_code_blocks: Rc<RefCell<HashMap<usize, u64>>>,
    streaming: Cell<bool>,
}

impl Default for MarkdownView {
    fn default() -> Self {
        Self::new()
    }
}

impl MarkdownView {
    pub fn new() -> Self {
        Self {
            parser: IncrementalParser::new(),
            tail: Vec::new(),
            flats: RefCell::new(HashMap::new()),
            volatile_from: Cell::new(0),
            style: Cell::new(None),
            veil: RefCell::new(RowVeil::default()),
            copied_code_blocks: Rc::new(RefCell::new(HashMap::new())),
            streaming: Cell::new(false),
        }
    }

    /// A view attached to an already-streaming body. Its first rendered text
    /// becomes the full-opacity baseline; later appends fade normally.
    pub fn seeded() -> Self {
        let view = Self::new();
        *view.veil.borrow_mut() = RowVeil::seeded();
        view
    }

    /// Reattach an existing parsed view without animating text that arrived
    /// while its session was off screen.
    pub fn seed_streaming_baseline(&self) {
        *self.veil.borrow_mut() = RowVeil::seeded();
    }

    /// Point the view at `text`. `mend` closes hanging inline markers, which is
    /// wanted while a response streams and not once it has settled.
    /// Bytes of source this view retains. Parsed structures run to roughly
    /// seventeen times this, so it is the honest unit for bounding a cache.
    pub fn source_len(&self) -> usize {
        self.parser.text().len()
    }

    pub fn set_text(&mut self, text: &str, mend: bool) {
        let was_streaming = self.streaming.replace(mend);
        if !mend && was_streaming {
            *self.veil.borrow_mut() = RowVeil::default();
        } else if mend && !was_streaming && !self.parser.text().is_empty() {
            // A completed body that starts streaming again already has a
            // rendered baseline. Do not make that history dissolve again.
            *self.veil.borrow_mut() = RowVeil::seeded();
        }
        let changed = self.parser.text() != text;
        if changed {
            self.parser.set_text(text);
        }
        // The mended display tail depends only on the source and the
        // streaming flag. Deriving it re-mends — and, with a hanging marker,
        // re-parses — the final block, and `set_text` runs for every visible
        // row on every frame, so a frame that changed neither input must not
        // pay for it.
        if changed || mend != was_streaming {
            let tail = if mend {
                self.parser.display_tail().unwrap_or_default()
            } else {
                Vec::new()
            };
            if changed || tail != self.tail {
                self.tail = tail;
                // Markdown block structure only ever extends the final block,
                // so every element before it is still valid. A streamed delta
                // thus re-flattens one block instead of the whole response.
                let boundary = self.volatile_from.get();
                self.flats
                    .borrow_mut()
                    .retain(|ordinal, _| *ordinal < boundary);
            }
        }
    }

    pub fn is_fading(&self) -> bool {
        self.streaming.get() && self.veil.borrow().is_fading()
    }

    /// Drop cached flats if the style they were built for no longer applies.
    fn sync_style(&self, palette: &Palette, metrics: &Metrics) {
        let current = (*palette, *metrics);
        if self.style.get() != Some(current) {
            self.style.set(Some(current));
            self.flats.borrow_mut().clear();
        }
    }

    /// Flattened inline content for the element at `ordinal`, built on miss.
    fn flat(&self, ordinal: usize, build: impl FnOnce() -> FlatText) -> Rc<FlatText> {
        self.flats
            .borrow_mut()
            .entry(ordinal)
            .or_insert_with(|| Rc::new(build()))
            .clone()
    }

    /// Display blocks in document order: the settled prefix, then the mended
    /// tail when one is active.
    fn blocks(&self) -> impl Iterator<Item = &Block> + '_ {
        let all = &self.parser.tree().blocks;
        let settled = if self.tail.is_empty() {
            all.len()
        } else {
            all.len().saturating_sub(1)
        };
        all[..settled]
            .iter()
            .chain(self.tail.iter())
            .map(|top| &top.block)
    }
}

// ── Render context ─────────────────────────────────────────────────────────

/// Everything a render pass needs, plus the element counter that assigns
/// document-ordered keys. Keys stay stable frame to frame as long as the block
/// structure does, which is what lets a selection survive scrolling.
pub struct Ctx<'a> {
    row: Rc<str>,
    palette: &'a Palette,
    metrics: Metrics,
    selection: TranscriptSelection,
    link_handler: Option<LinkHandler>,
    /// Cross-frame flatten cache, when this render has one to consult.
    cache: Option<&'a MarkdownView>,
    next_ordinal: Cell<usize>,
    /// Set while rendering the first element of a block, for copy spacing.
    starts_block: Cell<bool>,
    animate_streaming: bool,
    now: Instant,
}

impl<'a> Ctx<'a> {
    pub fn new(
        row: impl Into<Rc<str>>,
        palette: &'a Palette,
        metrics: Metrics,
        selection: TranscriptSelection,
    ) -> Self {
        Self {
            row: row.into(),
            palette,
            metrics,
            selection,
            link_handler: None,
            cache: None,
            next_ordinal: Cell::new(0),
            starts_block: Cell::new(true),
            animate_streaming: true,
            now: Instant::now(),
        }
    }

    pub fn selection(&self) -> &TranscriptSelection {
        &self.selection
    }

    pub fn with_link_handler(mut self, handler: LinkHandler) -> Self {
        self.link_handler = Some(handler);
        self
    }

    pub fn with_streaming_animation(mut self, animate: bool) -> Self {
        self.animate_streaming = animate;
        self
    }

    fn with_cache(&self, view: &'a MarkdownView) -> Self {
        Self {
            row: self.row.clone(),
            palette: self.palette,
            metrics: self.metrics,
            selection: self.selection.clone(),
            link_handler: self.link_handler.clone(),
            cache: Some(view),
            next_ordinal: Cell::new(self.next_ordinal.get()),
            starts_block: Cell::new(self.starts_block.get()),
            animate_streaming: self.animate_streaming,
            now: Instant::now(),
        }
    }

    fn next_key(&self) -> TextKey {
        let ordinal = self.next_ordinal.get();
        self.next_ordinal.set(ordinal + 1);
        TextKey::new(self.row.clone(), ordinal)
    }

    fn take_block_break(&self) -> bool {
        self.starts_block.replace(false)
    }

    /// Flatten through the cache when one is wired: a settled block reuses its
    /// string and `TextRun`s untouched, so an unchanged paragraph costs one
    /// `Rc` clone per frame instead of a fresh allocation.
    fn flat(&self, ordinal: usize, build: impl FnOnce() -> FlatText) -> Rc<FlatText> {
        match self.cache {
            Some(view) => view.flat(ordinal, build),
            None => Rc::new(build()),
        }
    }
}

// ── The shared text primitive ──────────────────────────────────────────────

/// One selectable, decorated text element.
///
/// The `canvas` underlay is an *earlier sibling* than the text, so GPUI paints
/// it first — underneath the glyphs — while the text's prepaint has already
/// filled in the shared [`TextLayout`]. That ordering is what lets a pure-paint
/// pass read real glyph geometry without a second layout pass.
fn text_element_with_selection(
    flat: &FlatText,
    runs: Vec<TextRun>,
    key: TextKey,
    selection: TranscriptSelection,
    link_handler: Option<LinkHandler>,
    code_wash: Hsla,
    selection_wash: Hsla,
    block_break: bool,
) -> AnyElement {
    let styled = StyledText::new(flat.text.clone()).with_runs(runs);
    let layout = styled.layout().clone();

    let body: AnyElement = if flat.links.is_empty() {
        styled.into_any_element()
    } else {
        let (ranges, urls): (Vec<_>, Vec<_>) = flat.links.iter().cloned().unzip();
        let id = SharedString::from(format!("{}-t{}", key.row, key.index));
        InteractiveText::new(id, styled)
            .on_click(ranges, move |clicked, window, cx| {
                if let Some(url) = urls.get(clicked) {
                    if let Some(handler) = &link_handler {
                        handler(url, window, cx);
                    } else {
                        cx.open_url(url);
                    }
                }
            })
            .into_any_element()
    };

    let underlay = canvas(|_, _, _| (), {
        let text = flat.text.clone();
        let code_ranges = flat.code_ranges.clone();
        let layout = layout.clone();
        let key = key.clone();
        move |_, _, window, _| {
            for range in &code_ranges {
                for rect in range_rects(&layout, range, CODE_WASH_PAD_X, CODE_WASH_INSET_Y) {
                    window.paint_quad(quad(
                        rect,
                        px(CODE_WASH_RADIUS),
                        code_wash,
                        px(0.0),
                        gpui::transparent_black(),
                        BorderStyle::default(),
                    ));
                }
            }
            if let Some(range) = selection.selection.borrow().wash_range(&key) {
                for rect in range_rects(&layout, &range, 0.0, 0.0) {
                    window.paint_quad(quad(
                        rect,
                        px(0.0),
                        selection_wash,
                        px(0.0),
                        gpui::transparent_black(),
                        BorderStyle::default(),
                    ));
                }
            }
            // Paint order is document order, so simply appending here
            // rebuilds the frame's selection continuity.
            selection.registry.borrow_mut().push(RegisteredText {
                key: key.clone(),
                text: Rc::from(text.as_ref()),
                block_break,
                geometry: layout.clone(),
            });
        }
    })
    .absolute()
    .size_full();

    div()
        .relative()
        .w_full()
        .min_w_0()
        .cursor(CursorStyle::IBeam)
        .child(underlay)
        .child(body)
        .into_any_element()
}

fn text_element(flat: &FlatText, key: TextKey, ctx: &Ctx) -> AnyElement {
    let runs = match ctx
        .cache
        .filter(|view| ctx.animate_streaming && view.streaming.get())
    {
        Some(view) => {
            let spans = view
                .veil
                .borrow_mut()
                .advance(key.index, flat.text.as_ref(), ctx.now);
            apply_veil(flat.runs.clone(), &spans)
        }
        None => flat.runs.clone(),
    };
    text_element_with_selection(
        flat,
        runs,
        key,
        ctx.selection.clone(),
        ctx.link_handler.clone(),
        ctx.palette.code_wash,
        ctx.palette.selection,
        ctx.take_block_break(),
    )
}

/// A selectable styled line outside the markdown block renderer.
///
/// Diff viewers and other virtualized code surfaces can share the transcript's
/// cross-element selection behavior without manufacturing a markdown tree.
/// The caller supplies a stable key in paint order and decides whether copying
/// across this element should insert a paragraph break or a single newline.
pub fn selectable_flat_text(
    flat: &FlatText,
    key: TextKey,
    selection: TranscriptSelection,
    code_wash: Hsla,
    selection_wash: Hsla,
    block_break: bool,
) -> AnyElement {
    text_element_with_selection(
        flat,
        flat.runs.clone(),
        key,
        selection,
        None,
        code_wash,
        selection_wash,
        block_break,
    )
}

/// A selectable plain-text element: user messages, tool output, anything that
/// is not markdown but still takes part in transcript-wide selection.
pub fn plain_text(
    text: impl Into<SharedString>,
    family: &'static str,
    weight: FontWeight,
    color: Hsla,
    ctx: &Ctx,
) -> AnyElement {
    let key = ctx.next_key();
    let flat = ctx.flat(key.index, || flatten_plain(text, family, weight, color));
    text_element(&flat, key, ctx)
}

/// A zero-size canvas that clears the frame's registry. Paint it *before* any
/// transcript text so the registry holds exactly this frame's visible elements.
pub fn frame_reset(selection: TranscriptSelection) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |_, _, _, _| selection.registry.borrow_mut().clear(),
    )
    .absolute()
    .w(px(0.0))
    .h(px(0.0))
}

// ── Selection geometry and input ───────────────────────────────────────────

/// Wash boxes for one byte range: one box per visual row the range covers, in
/// window coordinates, from the laid-out text's own geometry. `pad_x` overhangs
/// horizontally (inline code) and `inset_y` shrinks vertically; a selection
/// wash passes zero for both so its boxes tile seamlessly across wrapped rows.
fn range_rects(
    layout: &TextLayout,
    range: &Range<usize>,
    pad_x: f32,
    inset_y: f32,
) -> Vec<Bounds<Pixels>> {
    let mut rects = Vec::new();
    if range.is_empty() || layout_missing(layout) {
        return rects;
    }

    let bounds = layout.bounds();
    let line_height = layout.line_height();
    let mut row_top = bounds.top();
    let mut line_start = 0;

    // A soft-wrap boundary belongs to both adjacent rows, but GPUI's generic
    // `position_for_index` gives it the preceding row's caret affinity. Walking
    // with that API therefore has to jump beyond the boundary to make progress,
    // dropping the first glyph of every continuation row. Use the shaped wrap
    // boundaries directly, as Zed's Markdown renderer does, so adjacent visual
    // rows share the exact same byte boundary without a gap.
    for line in layout.line_layouts() {
        let line_end = line_start + line.len();
        let unwrapped = &line.unwrapped_layout;
        let row_ends = line
            .wrap_boundaries()
            .iter()
            .map(|boundary| {
                let glyph = &unwrapped.runs[boundary.run_ix].glyphs[boundary.glyph_ix];
                (line_start + glyph.index, glyph.position.x)
            })
            .chain([(line_end, unwrapped.width)]);
        let mut row_start = line_start;
        let mut row_start_x = Pixels::ZERO;

        for (row_end, row_end_x) in row_ends {
            let selected_start = range.start.max(row_start);
            let selected_end = range.end.min(row_end);
            if selected_start < selected_end {
                let x_for_index =
                    |index| bounds.left() + unwrapped.x_for_index(index - line_start) - row_start_x;
                let start_x = x_for_index(selected_start);
                let end_x = x_for_index(selected_end);
                if end_x > start_x {
                    rects.push(Bounds::new(
                        point(start_x - px(pad_x), row_top + px(inset_y)),
                        size(
                            end_x - start_x + px(2.0 * pad_x),
                            line_height - px(2.0 * inset_y),
                        ),
                    ));
                }
            }

            row_start = row_end;
            row_start_x = row_end_x;
            row_top += line_height;
        }

        // `TextLayout` separates hard lines with one newline byte, which has
        // no glyph box of its own.
        line_start = line_end + 1;
        if line_start > range.end {
            break;
        }
    }
    rects
}

/// `TextLayout::bounds` panics before prepaint has run. A row that was spliced
/// this frame can reach paint with a fresh layout, so probe first.
fn layout_missing(layout: &TextLayout) -> bool {
    layout.line_layouts().is_empty()
}

/// The registry entry containing `position`, else the nearest by vertical
/// distance so a drag through a gutter or between blocks clamps sensibly.
fn registry_point(
    registry: &SelectionRegistry<TextGeometry>,
    position: Point<Pixels>,
) -> Option<(usize, usize)> {
    let mut best: Option<(usize, f32)> = None;
    for (index, entry) in registry.entries().iter().enumerate() {
        if layout_missing(&entry.geometry) {
            continue;
        }
        let bounds = entry.geometry.bounds();
        let distance = if position.y < bounds.top() {
            f32::from(bounds.top() - position.y)
        } else if position.y > bounds.bottom() {
            f32::from(position.y - bounds.bottom())
        } else {
            0.0
        };
        if best.is_none_or(|(_, best)| distance < best) {
            best = Some((index, distance));
        }
        if distance == 0.0 {
            break;
        }
    }
    let (index, _) = best?;
    let offset = match registry.entries()[index]
        .geometry
        .index_for_position(position)
    {
        Ok(offset) | Err(offset) => offset,
    };
    Some((index, offset))
}

/// Install the frame's selection mouse listeners.
///
/// These live once per frame at the transcript root rather than once per
/// painted text element: the registry already holds every element's geometry,
/// so three closures replace three-per-element and a mouse move costs one
/// registry scan instead of one dispatch per visible paragraph.
pub fn install_selection_input(window: &mut Window, state: &TranscriptSelection) {
    window.on_mouse_event({
        let state = state.clone();
        move |event: &MouseDownEvent, phase, window, _| {
            if phase != DispatchPhase::Bubble || event.button != MouseButton::Left {
                return;
            }
            let registry = state.registry.borrow();
            let hit = registry.entries().iter().enumerate().find(|(_, entry)| {
                !layout_missing(&entry.geometry)
                    && entry.geometry.bounds().contains(&event.position)
            });
            let mut selection = state.selection.borrow_mut();
            match hit {
                Some((_, entry)) => {
                    let offset = match entry.geometry.index_for_position(event.position) {
                        Ok(offset) | Err(offset) => offset,
                    };
                    match event.click_count {
                        2 => selection.begin_with_span(
                            entry.key.clone(),
                            entry.text.clone(),
                            word_range(&entry.text, offset),
                        ),
                        count if count >= 3 => selection.begin_with_span(
                            entry.key.clone(),
                            entry.text.clone(),
                            line_range(&entry.text, offset),
                        ),
                        _ => selection.begin(entry.key.clone(), offset),
                    }
                    drop(selection);
                    drop(registry);
                    window.refresh();
                }
                None => {
                    let had_selection = !selection.is_empty();
                    selection.clear();
                    drop(selection);
                    drop(registry);
                    if had_selection {
                        window.refresh();
                    }
                }
            }
        }
    });

    window.on_mouse_event({
        let state = state.clone();
        move |event: &MouseMoveEvent, phase, window, _| {
            if phase != DispatchPhase::Bubble || !event.dragging() {
                return;
            }
            let registry = state.registry.borrow();
            let anchor = {
                let selection = state.selection.borrow();
                selection
                    .anchor()
                    .cloned()
                    .and_then(|key| selection.drag_anchor(&key).map(|offset| (key, offset)))
                    .and_then(|(key, offset)| registry.position(&key).map(|index| (index, offset)))
            };
            // The anchor scrolling out of the frame keeps the existing spans
            // rather than collapsing the selection.
            let Some((anchor_index, anchor_offset)) = anchor else {
                return;
            };
            let Some(head) = registry_point(&registry, event.position) else {
                return;
            };
            let spans = registry.resolve((anchor_index, anchor_offset), head);
            drop(registry);
            if state.selection.borrow_mut().set_spans(spans) {
                window.refresh();
            }
        }
    });

    window.on_mouse_event({
        let state = state.clone();
        move |_: &MouseUpEvent, phase, _, _| {
            if phase != DispatchPhase::Bubble {
                return;
            }
            let key = state.selection.borrow().anchor().cloned();
            if let Some(key) = key {
                state.selection.borrow_mut().end_drag(&key);
            }
        }
    });
}

// ── Blocks ─────────────────────────────────────────────────────────────────

/// Per-top-level-block ordinal stride: an element's ordinal is
/// `block_index << 16 | position_within_block`. Deriving keys from the
/// block's document index rather than a running document counter means a
/// walk that skips leading blocks ([`markdown_tail`]) hands every rendered
/// block exactly the flatten-cache and veil keys a full walk would, so the
/// two can alternate without thrashing either.
const BLOCK_ORDINAL_STRIDE_BITS: u32 = 16;

fn block_ordinal_base(block_ix: usize) -> usize {
    block_ix << BLOCK_ORDINAL_STRIDE_BITS
}

/// Render a markdown body. Returns `None` when it has no content.
pub fn markdown<'a>(view: &'a MarkdownView, ctx: &Ctx<'a>) -> Option<AnyElement> {
    markdown_capped(view, ctx, usize::MAX)
}

/// Like [`markdown`], but builds only the trailing `max_blocks` top-level
/// blocks. The live reasoning peek shows a tail-pinned viewport while a
/// thought streams, and building the whole growing document every pulse tick
/// made a long think O(document) per frame; the cap makes it O(window).
pub fn markdown_tail<'a>(
    view: &'a MarkdownView,
    ctx: &Ctx<'a>,
    max_blocks: usize,
) -> Option<AnyElement> {
    markdown_capped(view, ctx, max_blocks.max(1))
}

fn markdown_capped<'a>(
    view: &'a MarkdownView,
    ctx: &Ctx<'a>,
    max_blocks: usize,
) -> Option<AnyElement> {
    let blocks = view.blocks().collect::<Vec<_>>();
    let Some((&last, leading)) = blocks.split_last() else {
        if ctx.animate_streaming && view.streaming.get() {
            let mut veil = view.veil.borrow_mut();
            veil.begin_frame();
            veil.finish_frame();
        }
        return None;
    };

    view.sync_style(ctx.palette, &ctx.metrics);
    let ctx = ctx.with_cache(view);
    if ctx.animate_streaming && view.streaming.get() {
        view.veil.borrow_mut().begin_frame();
    }
    let first = blocks.len().saturating_sub(max_blocks);
    let mut children = Vec::with_capacity(blocks.len() - first);
    for (block_ix, block) in leading.iter().enumerate().skip(first) {
        ctx.next_ordinal.set(block_ordinal_base(block_ix));
        children.push(render_block(block, &ctx));
        debug_assert!(
            ctx.next_ordinal.get() - block_ordinal_base(block_ix) < 1 << BLOCK_ORDINAL_STRIDE_BITS,
            "a single block overflowed its ordinal stride"
        );
    }
    // Everything before the final block is settled, so its flattened elements
    // stay cacheable across appends.
    let last_base = block_ordinal_base(blocks.len() - 1);
    ctx.next_ordinal.set(last_base);
    view.volatile_from.set(last_base);
    children.push(render_block(last, &ctx));
    if ctx.animate_streaming && view.streaming.get() {
        // Every element visible on the attach pass has synchronously adopted
        // its baseline. Elements introduced by later appends should now fade.
        view.veil.borrow_mut().finish_frame();
    }

    Some(
        div()
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(ctx.metrics.block_gap))
            .children(children)
            .into_any_element(),
    )
}

fn render_block(block: &Block, ctx: &Ctx) -> AnyElement {
    ctx.starts_block.set(true);
    match block {
        Block::Paragraph { runs } => {
            let key = ctx.next_key();
            let flat = ctx.flat(key.index, || {
                flatten(runs, ctx.palette, FontWeight::NORMAL, ctx.palette.text)
            });
            div()
                .w_full()
                .min_w_0()
                .text_size(px(ctx.metrics.text_size))
                .line_height(px(ctx.metrics.line_height))
                .child(text_element(&flat, key, ctx))
                .into_any_element()
        }
        Block::Heading { level, runs } => {
            let (size, line_height, weight) = heading_metrics(*level, &ctx.metrics);
            let key = ctx.next_key();
            let flat = ctx.flat(key.index, || {
                flatten(runs, ctx.palette, weight, ctx.palette.text)
            });
            div()
                .w_full()
                .min_w_0()
                .when(*level <= 2, |element| element.pt(px(4.0)))
                .text_size(px(size))
                .line_height(px(line_height))
                .child(text_element(&flat, key, ctx))
                .into_any_element()
        }
        Block::Image { url, alt } => render_image(url, alt, ctx),
        Block::CodeBlock { language, code } => render_code_block(language.as_deref(), code, ctx),
        Block::BlockQuote { children } => {
            let rendered = children
                .iter()
                .map(|child| render_block(child, ctx))
                .collect::<Vec<_>>();
            div()
                .w_full()
                .min_w_0()
                .flex()
                .gap(px(10.0))
                .child(
                    div()
                        .w(px(2.0))
                        .flex_none()
                        .rounded_full()
                        .bg(ctx.palette.border),
                )
                .child(
                    div()
                        // flex_auto, not flex_1: a zero flex-basis erases the
                        // content's intrinsic width, collapsing shrink-wrapped
                        // user bubbles to the quote bar.
                        .flex_auto()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(ctx.metrics.block_gap))
                        .children(rendered),
                )
                .into_any_element()
        }
        Block::List {
            ordered_start,
            items,
        } => render_list(*ordered_start, items, ctx),
        Block::Table {
            header,
            rows,
            align,
        } => render_table(header, rows, align, ctx),
        Block::Rule => div()
            .w_full()
            .h(px(1.0))
            .my(px(4.0))
            .bg(ctx.palette.border)
            .into_any_element(),
    }
}

fn render_list(ordered_start: Option<u64>, items: &[ListItem], ctx: &Ctx) -> AnyElement {
    let marker_width = if ordered_start.is_some() { 22.0 } else { 14.0 };
    let rendered = items
        .iter()
        .enumerate()
        .map(|(index, item)| {
            let marker = match (ordered_start, item.task) {
                (_, Some(checked)) => div()
                    .w(px(marker_width))
                    .flex_none()
                    .flex()
                    .justify_start()
                    .child(checkbox(checked, ctx))
                    .into_any_element(),
                (Some(start), None) => {
                    marker_text(format!("{}.", start + index as u64), marker_width, ctx)
                }
                (None, None) => marker_text("•".to_owned(), marker_width, ctx),
            };
            let blocks = item
                .blocks
                .iter()
                .map(|block| render_block(block, ctx))
                .collect::<Vec<_>>();
            div()
                .w_full()
                .min_w_0()
                .flex()
                .items_start()
                .child(marker)
                .child(
                    div()
                        // flex_auto, not flex_1: a zero flex-basis erases the
                        // content's intrinsic width, collapsing shrink-wrapped
                        // user bubbles to the marker column.
                        .flex_auto()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(ctx.metrics.block_gap * 0.6))
                        .children(blocks),
                )
                .into_any_element()
        })
        .collect::<Vec<_>>();

    div()
        .w_full()
        .min_w_0()
        .flex()
        .flex_col()
        .gap(px(ctx.metrics.block_gap * 0.5))
        .children(rendered)
        .into_any_element()
}

/// A list marker. Markers are not selectable: they are generated ornament, not
/// content the user typed, so they stay out of the selection registry.
fn marker_text(label: String, width: f32, ctx: &Ctx) -> AnyElement {
    div()
        .w(px(width))
        .flex_none()
        .text_size(px(ctx.metrics.text_size))
        .line_height(px(ctx.metrics.line_height))
        .text_color(ctx.palette.tertiary)
        .child(SharedString::from(label))
        .into_any_element()
}

fn checkbox(checked: bool, ctx: &Ctx) -> AnyElement {
    let box_size = (ctx.metrics.text_size * 0.92).round();
    div()
        .size(px(box_size))
        .my(px(((ctx.metrics.line_height - box_size) / 2.0).max(0.0)))
        .flex_none()
        .rounded(px(3.0))
        .border_1()
        .border_color(if checked {
            ctx.palette.accent
        } else {
            ctx.palette.border
        })
        .when(checked, |element| element.bg(ctx.palette.accent))
        .flex()
        .items_center()
        .justify_center()
        .when(checked, |element| {
            element.child(crate::ui::icon(
                "icons/check.svg",
                box_size - 4.0,
                ctx.palette.inset,
            ))
        })
        .into_any_element()
}

/// An inline image. Data URLs decode in place; anything else is handed to GPUI
/// to load. The alt text renders beneath as a caption when there is one, so a
/// failed or slow load still says what it was.
fn render_image(url: &str, alt: &str, ctx: &Ctx) -> AnyElement {
    const MAX_HEIGHT: f32 = 320.0;

    let key = ctx.next_key();
    let id = SharedString::from(format!("image-{}-{}", key.row, key.index));
    let image = match decode_data_url(url) {
        Some(decoded) => img(decoded).id(id),
        None => img(url.to_owned()).id(id),
    };
    div()
        .w_full()
        .min_w_0()
        .flex()
        .flex_col()
        .gap(px(4.0))
        .child(
            image
                .max_w(relative(1.0))
                .max_h(px(MAX_HEIGHT))
                .rounded(px(6.0))
                .object_fit(gpui::ObjectFit::ScaleDown),
        )
        .when(!alt.trim().is_empty(), |element| {
            element.child(
                div()
                    .text_size(px(ctx.metrics.text_size - 2.0))
                    .line_height(px(ctx.metrics.line_height - 4.0))
                    .text_color(ctx.palette.ghost)
                    .child(SharedString::from(alt.to_owned())),
            )
        })
        .into_any_element()
}

const CODE_COPY_FEEDBACK_DURATION: Duration = Duration::from_secs(3);
type CodeCopyFeedback = Rc<RefCell<HashMap<usize, u64>>>;

fn begin_code_copy_feedback(feedback: &CodeCopyFeedback, ordinal: usize) -> u64 {
    let mut feedback = feedback.borrow_mut();
    let generation = feedback
        .get(&ordinal)
        .copied()
        .unwrap_or_default()
        .wrapping_add(1);
    feedback.insert(ordinal, generation);
    generation
}

fn clear_code_copy_feedback(feedback: &CodeCopyFeedback, ordinal: usize, generation: u64) -> bool {
    let mut feedback = feedback.borrow_mut();
    if feedback.get(&ordinal) != Some(&generation) {
        return false;
    }
    feedback.remove(&ordinal);
    true
}

fn show_code_copied(feedback: CodeCopyFeedback, ordinal: usize, cx: &mut gpui::App) {
    let generation = begin_code_copy_feedback(&feedback, ordinal);
    cx.refresh_windows();
    cx.spawn(async move |cx| {
        cx.background_executor()
            .timer(CODE_COPY_FEEDBACK_DURATION)
            .await;
        if clear_code_copy_feedback(&feedback, ordinal, generation) {
            cx.refresh();
        }
    })
    .detach();
}

/// Decode a `data:` image URL. Shared with the transcript's tool-output images.
pub fn decode_data_url(url: &str) -> Option<std::sync::Arc<gpui::Image>> {
    use base64::Engine as _;

    let (header, encoded) = url.split_once(',')?;
    let mime_type = header.strip_prefix("data:")?.split(';').next()?;
    let format = gpui::ImageFormat::from_mime_type(mime_type)?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .ok()?;
    (!bytes.is_empty()).then(|| std::sync::Arc::new(gpui::Image::from_bytes(format, bytes)))
}

fn render_code_block(language: Option<&str>, code: &str, ctx: &Ctx) -> AnyElement {
    let key = ctx.next_key();
    // Tokenizing is the most expensive flatten in the document, so a settled
    // code block is exactly the case the cache exists for.
    let flat = ctx.flat(key.index, || {
        let lang = language.and_then(highlight::lang_for_tag);
        let mut code_font = font(MONO_FAMILY);
        code_font.weight = FontWeight::NORMAL;
        FlatText {
            text: SharedString::from(code.to_owned()),
            runs: code_runs(code, lang, &code_font, ctx.palette),
            links: Vec::new(),
            code_ranges: Vec::new(),
        }
    });
    let label = language
        .filter(|language| !language.is_empty())
        .map(|language| language.to_ascii_lowercase());
    // Reuse the cached shaped string. Settled code blocks render every frame,
    // so cloning the whole source here would turn the copy affordance into a
    // permanent O(code length) render cost; allocate only when it is invoked.
    let copy_content = flat.text.clone();
    let keyboard_copy_content = copy_content.clone();
    let copy_feedback = ctx.cache.map(|view| view.copied_code_blocks.clone());
    let copied = copy_feedback
        .as_ref()
        .is_some_and(|feedback| feedback.borrow().contains_key(&key.index));
    let keyboard_copy_feedback = copy_feedback.clone();
    let ordinal = key.index;
    let copy_button = div()
        .id(SharedString::from(format!(
            "copy-code-{}-{}",
            key.row, key.index
        )))
        .tab_index(0)
        .size(px(24.0))
        .flex_none()
        .rounded(px(5.0))
        .flex()
        .items_center()
        .justify_center()
        .cursor_default()
        .focus_visible(|style| style.border_1().border_color(ctx.palette.accent))
        .hover(|style| style.bg(ctx.palette.overlay))
        .child(crate::ui::icon(
            if copied {
                "icons/check.svg"
            } else {
                "icons/copy.svg"
            },
            11.0,
            ctx.palette.ghost,
        ))
        .tooltip(Tooltip::text(if copied {
            tr!("common.copied")
        } else {
            tr!("common.copy_code")
        }))
        .on_click(move |_, _, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(copy_content.to_string()));
            if let Some(feedback) = copy_feedback.clone() {
                show_code_copied(feedback, ordinal, cx);
            }
        })
        .on_key_down(move |event: &KeyDownEvent, _, cx| {
            if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                cx.write_to_clipboard(ClipboardItem::new_string(keyboard_copy_content.to_string()));
                if let Some(feedback) = keyboard_copy_feedback.clone() {
                    show_code_copied(feedback, ordinal, cx);
                }
                cx.stop_propagation();
            }
        });

    div()
        .id(SharedString::from(format!(
            "code-block-{}-{}",
            key.row, key.index
        )))
        .tab_group()
        .tab_stop(false)
        .w_full()
        .min_w_0()
        .rounded(px(8.0))
        .border_1()
        .border_color(ctx.palette.border)
        .bg(ctx.palette.inset)
        .overflow_hidden()
        .child(
            div()
                .w_full()
                .h(px(28.0))
                .pl(px(10.0))
                .pr(px(2.0))
                .flex()
                .items_center()
                .border_b_1()
                .border_color(ctx.palette.border)
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .truncate()
                        .text_size(px(10.0))
                        .line_height(px(14.0))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(ctx.palette.ghost)
                        .when_some(label, |element, label| {
                            element.child(SharedString::from(label))
                        }),
                )
                .child(copy_button),
        )
        .child(
            div()
                .id(SharedString::from(format!(
                    "code-{}-{}",
                    key.row, key.index
                )))
                .w_full()
                .min_w_0()
                .px(px(10.0))
                .py(px(8.0))
                .child(
                    div()
                        .w_full()
                        .min_w_0()
                        .whitespace_normal()
                        .text_size(px(ctx.metrics.code_text_size))
                        .line_height(px(ctx.metrics.code_line_height))
                        .text_color(ctx.palette.secondary)
                        .child(text_element(&flat, key, ctx)),
                ),
        )
        .into_any_element()
}

/// `TextRun`s that tile `code` exactly, colored by the lexer. Every run shares
/// one font, so the shaped width of a line is identical with or without
/// highlighting — the property that makes coloring safe to defer.
fn code_runs(code: &str, lang: Option<Lang>, code_font: &Font, palette: &Palette) -> Vec<TextRun> {
    let plain = palette.secondary;
    let mut runs: Vec<TextRun> = Vec::new();
    let push = |runs: &mut Vec<TextRun>, len: usize, color: Hsla| {
        if len == 0 {
            return;
        }
        match runs.last_mut() {
            Some(last) if last.color == color => last.len += len,
            _ => runs.push(TextRun {
                len,
                font: code_font.clone(),
                color,
                background_color: None,
                underline: None,
                strikethrough: None,
            }),
        }
    };

    let tokenized = lang.map(|lang| highlight::tokenize(lang, code));
    let lines = code.split('\n').collect::<Vec<_>>();
    for (index, line) in lines.iter().enumerate() {
        let tokens = tokenized
            .as_ref()
            .and_then(|lines| lines.get(index))
            .map(Vec::as_slice)
            .unwrap_or_default();
        let mut cursor = 0;
        for token in tokens {
            push(&mut runs, token.range.start.saturating_sub(cursor), plain);
            push(&mut runs, token.range.len(), palette.token(token.class));
            cursor = token.range.end;
        }
        push(&mut runs, line.len().saturating_sub(cursor), plain);
        if index + 1 < lines.len() {
            // The '\n' separator must belong to a run or shaping rejects them.
            push(&mut runs, 1, plain);
        }
    }
    runs
}

fn render_table(
    header: &[Vec<InlineRun>],
    rows: &[Vec<Vec<InlineRun>>],
    align: &[TableAlign],
    ctx: &Ctx,
) -> AnyElement {
    let columns = header
        .len()
        .max(rows.iter().map(Vec::len).max().unwrap_or(0));
    if columns == 0 {
        return div().into_any_element();
    }
    let widths = column_widths(header, rows, columns);

    let mut table = div()
        .w_full()
        .min_w_0()
        .rounded(px(8.0))
        .border_1()
        .border_color(ctx.palette.border)
        .overflow_hidden()
        .flex()
        .flex_col();

    if !header.is_empty() {
        table = table.child(
            table_row(header, &widths, align, ctx, FontWeight::SEMIBOLD, true)
                .bg(ctx.palette.overlay),
        );
    }
    for (index, row) in rows.iter().enumerate() {
        table = table.child(table_row(
            row,
            &widths,
            align,
            ctx,
            FontWeight::NORMAL,
            index + 1 < rows.len(),
        ));
    }
    table.into_any_element()
}

fn table_row(
    cells: &[Vec<InlineRun>],
    widths: &[f32],
    align: &[TableAlign],
    ctx: &Ctx,
    weight: FontWeight,
    divider: bool,
) -> gpui::Div {
    let mut row = div()
        .w_full()
        .min_w_0()
        .flex()
        .items_start()
        .when(divider, |element| {
            element.border_b_1().border_color(ctx.palette.border)
        });
    for (index, cell) in cells.iter().enumerate() {
        let key = ctx.next_key();
        let flat = ctx.flat(key.index, || {
            flatten(cell, ctx.palette, weight, ctx.palette.text)
        });
        let alignment = align.get(index).copied().unwrap_or_default();
        row = row.child(
            div()
                .w(relative(widths.get(index).copied().unwrap_or(0.0)))
                .min_w_0()
                .px(px(9.0))
                .py(px(6.0))
                .text_size(px(ctx.metrics.text_size - 0.5))
                .line_height(px(ctx.metrics.line_height - 2.0))
                .map(|element| match alignment {
                    TableAlign::Left => element,
                    TableAlign::Center => element.items_center().text_center(),
                    TableAlign::Right => element.items_end().text_right(),
                })
                .child(text_element(&flat, key, ctx)),
        );
    }
    row
}

/// Content-proportional column widths as fractions of the table, floored so a
/// narrow column stays readable.
fn column_widths(
    header: &[Vec<InlineRun>],
    rows: &[Vec<Vec<InlineRun>>],
    columns: usize,
) -> Vec<f32> {
    const MIN_FRACTION_SCALE: f32 = 0.55;

    let mut content = vec![0.0f32; columns];
    let mut note = |index: usize, cell: &Vec<InlineRun>| {
        if let Some(slot) = content.get_mut(index) {
            let length = cell
                .iter()
                .map(|run| run.text.chars().count())
                .sum::<usize>();
            *slot = slot.max(length as f32);
        }
    };
    for (index, cell) in header.iter().enumerate() {
        note(index, cell);
    }
    for row in rows {
        for (index, cell) in row.iter().enumerate() {
            note(index, cell);
        }
    }

    let even = 1.0 / columns as f32;
    let floor = even * MIN_FRACTION_SCALE;
    if content.iter().sum::<f32>() <= 0.0 {
        return vec![even; columns];
    }

    // Water-fill rather than clamp-then-renormalise: renormalising after a
    // clamp erodes the very floor it just applied. Each pass pins whatever fell
    // under the floor at exactly the floor and shares the remaining budget
    // among the rest, so the fractions still sum to one and every column clears
    // the floor. Terminates in at most `columns` passes.
    let mut widths = vec![even; columns];
    let mut pinned = vec![false; columns];
    loop {
        let free = (0..columns).filter(|index| !pinned[*index]).count();
        if free == 0 {
            break;
        }
        let budget = 1.0 - floor * (columns - free) as f32;
        let free_content = (0..columns)
            .filter(|index| !pinned[*index])
            .map(|index| content[index])
            .sum::<f32>();
        let mut pinned_any = false;
        for index in 0..columns {
            if pinned[index] {
                continue;
            }
            widths[index] = if free_content > 0.0 {
                budget * content[index] / free_content
            } else {
                budget / free as f32
            };
            if widths[index] < floor {
                widths[index] = floor;
                pinned[index] = true;
                pinned_any = true;
            }
        }
        if !pinned_any {
            break;
        }
    }
    widths
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::md::parser;
    use gpui::TestAppContext;

    fn palette() -> Palette {
        Palette::from_theme(&Theme::dark())
    }

    fn runs_of(source: &str) -> Vec<InlineRun> {
        match &parser::parse(source).blocks[0].block {
            Block::Paragraph { runs } => runs.clone(),
            other => panic!("expected a paragraph, got {other:?}"),
        }
    }

    /// `StyledText::with_runs` panics unless the runs tile the text exactly.
    fn assert_runs_tile(flat: &FlatText) {
        let total = flat.runs.iter().map(|run| run.len).sum::<usize>();
        assert_eq!(
            total,
            flat.text.len(),
            "runs must tile the text exactly: {:?}",
            flat.text
        );
    }

    #[test]
    fn flattened_runs_tile_the_text_and_carry_styles() {
        let flat = flatten(
            &runs_of("plain **bold** `code` [link](https://example.com) ~~gone~~"),
            &palette(),
            FontWeight::NORMAL,
            palette().text,
        );
        assert_runs_tile(&flat);
        assert_eq!(flat.text.as_ref(), "plain bold code link gone");
        assert_eq!(flat.links.len(), 1);
        assert_eq!(&flat.text[flat.links[0].0.clone()], "link");
        assert_eq!(flat.links[0].1, "https://example.com");
        assert_eq!(flat.code_ranges.len(), 1);
        assert_eq!(&flat.text[flat.code_ranges[0].clone()], "code");
        assert!(
            flat.runs
                .iter()
                .any(|run| run.strikethrough.is_some() && run.len == 4)
        );
        assert!(flat.runs.iter().any(|run| run.underline.is_some()));
    }

    #[test]
    fn a_streaming_link_is_styled_but_not_clickable() {
        let flat = flatten(
            &runs_of(&format!("see [docs]({PENDING_LINK_URL})")),
            &palette(),
            FontWeight::NORMAL,
            palette().text,
        );
        assert_runs_tile(&flat);
        assert!(
            flat.links.is_empty(),
            "the pending sentinel must not register a clickable range"
        );
        assert!(
            flat.runs.iter().any(|run| run.underline.is_some()),
            "but it should still look like a link"
        );
    }

    #[test]
    fn adjacent_code_and_link_runs_merge_into_one_range() {
        let flat = flatten(
            &runs_of("[**a** `b`](https://x) tail"),
            &palette(),
            FontWeight::NORMAL,
            palette().text,
        );
        assert_runs_tile(&flat);
        assert_eq!(flat.links.len(), 1, "one link, not one per styled run");
        assert_eq!(&flat.text[flat.links[0].0.clone()], "a b");
    }

    #[test]
    fn plain_flatten_tiles_and_handles_empty_text() {
        let flat = flatten_plain("hello", MONO_FAMILY, FontWeight::NORMAL, palette().text);
        assert_runs_tile(&flat);
        assert_eq!(flat.runs.len(), 1);

        let empty = flatten_plain("", SANS_FAMILY, FontWeight::NORMAL, palette().text);
        assert_runs_tile(&empty);
        assert!(empty.runs.is_empty());
    }

    #[test]
    fn code_runs_tile_the_block_including_newlines() {
        let code = "fn main() {\n    let x = 1; // c\n}";
        let mut code_font = font(MONO_FAMILY);
        code_font.weight = FontWeight::NORMAL;
        let runs = code_runs(code, Some(Lang::Rust), &code_font, &palette());
        assert_eq!(
            runs.iter().map(|run| run.len).sum::<usize>(),
            code.len(),
            "code runs must tile the whole block, newlines included"
        );
        assert!(runs.len() > 1, "highlighting should produce several runs");

        // Without a language the block is one plain run of the same length.
        let plain = code_runs(code, None, &code_font, &palette());
        assert_eq!(plain.iter().map(|run| run.len).sum::<usize>(), code.len());
        assert_eq!(plain.len(), 1);
    }

    #[test]
    fn code_block_rendering_wraps_and_exposes_a_keyboard_copy_control() {
        let source = include_str!("render.rs");
        let start = source
            .find("\nfn render_code_block(")
            .expect("code block renderer");
        let body = &source[start + 1..];
        let end = body
            .find("\nfn code_runs(")
            .expect("code block renderer end");
        let body = &body[..end];

        assert!(body.contains(".whitespace_normal()"));
        assert!(!body.contains(".overflow_x_scroll()"));
        assert!(!body.contains(".whitespace_nowrap()"));
        assert!(body.contains("\"icons/copy.svg\""));
        assert!(body.contains("\"icons/check.svg\""));
        assert!(body.contains("ClipboardItem::new_string"));
        assert!(body.contains("show_code_copied"));
        assert!(body.contains(".tab_index(0)"));
        assert!(body.contains(".on_key_down"));
    }

    #[test]
    fn copied_code_feedback_resets_after_three_seconds_and_ignores_stale_timers() {
        assert_eq!(CODE_COPY_FEEDBACK_DURATION, Duration::from_secs(3));

        let feedback = Rc::new(RefCell::new(HashMap::new()));
        let first = begin_code_copy_feedback(&feedback, 4);
        let second = begin_code_copy_feedback(&feedback, 4);

        assert!(!clear_code_copy_feedback(&feedback, 4, first));
        assert!(feedback.borrow().contains_key(&4));
        assert!(clear_code_copy_feedback(&feedback, 4, second));
        assert!(!feedback.borrow().contains_key(&4));
    }

    /// A soft-wrap boundary has two caret affinities. GPUI's generic
    /// `position_for_index` resolves it to the preceding row, so selection
    /// geometry must use the wrapped rows themselves or it can skip the first
    /// glyph on every continuation row.
    #[gpui::test]
    fn wrapped_selection_starts_at_each_continuation_row_origin(cx: &mut TestAppContext) {
        struct TestWindow;

        impl gpui::Render for TestWindow {
            fn render(&mut self, _: &mut Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
                div()
            }
        }

        let (_, cx) = cx.add_window_view(|_, _| TestWindow);
        let text: SharedString =
            "one two three four five six seven eight nine ten eleven twelve".into();
        let styled = StyledText::new(text.clone());
        let layout = styled.layout().clone();

        cx.draw(Point::default(), size(px(96.0), px(400.0)), move |_, _| {
            div()
                .w(px(96.0))
                .text_size(px(14.0))
                .line_height(px(20.0))
                .child(styled)
        });

        let rects = range_rects(&layout, &(0..text.len()), 0.0, 0.0);
        assert!(rects.len() >= 3, "fixture must wrap across several rows");
        let left = layout.bounds().left();
        assert!(
            rects.iter().all(|rect| rect.left() == left),
            "a full selection must include each wrapped row's first glyph: {rects:?}"
        );
    }

    /// Highlighting must never change the shaped length of a code block, or a
    /// deferred colorize would reflow the row.
    #[test]
    fn highlighting_never_changes_run_lengths() {
        let code = "const a = `t ${b}`;\n// note\nlet n = 0x1F;";
        let mut code_font = font(MONO_FAMILY);
        code_font.weight = FontWeight::NORMAL;
        let highlighted = code_runs(code, Some(Lang::Script), &code_font, &palette());
        let plain = code_runs(code, None, &code_font, &palette());
        assert_eq!(
            highlighted.iter().map(|run| run.len).sum::<usize>(),
            plain.iter().map(|run| run.len).sum::<usize>()
        );
        assert!(highlighted.iter().all(|run| &run.font == &code_font));
    }

    #[test]
    fn markdown_view_reuses_settled_elements_across_appends() {
        fn stub(label: &str) -> FlatText {
            flatten_plain(
                label.to_owned(),
                SANS_FAMILY,
                FontWeight::NORMAL,
                palette().text,
            )
        }

        let mut view = MarkdownView::new();
        view.set_text("First block.\n\nSecond bl", true);
        // Stand in for a render pass: two blocks, the second still streaming.
        let settled = view.flat(0, || stub("a"));
        let streaming = view.flat(1, || stub("b"));
        view.volatile_from.set(1);

        view.set_text("First block.\n\nSecond block.", true);

        // The settled block is reused by identity — no re-flatten, no alloc.
        let after = view.flat(0, || panic!("a settled block must not be rebuilt"));
        assert!(Rc::ptr_eq(&settled, &after));

        // The block that changed is rebuilt.
        let rebuilt = view.flat(1, || stub("c"));
        assert!(!Rc::ptr_eq(&streaming, &rebuilt));
        assert_eq!(rebuilt.text.as_ref(), "c");
    }

    /// Colors live inside `TextRun`s, so a theme switch has to drop the cache
    /// or the transcript keeps painting the previous palette.
    #[test]
    fn a_style_change_drops_cached_flats() {
        let view = MarkdownView::new();
        let dark = Palette::from_theme(&Theme::dark());
        let light = Palette::from_theme(&Theme::light());

        view.sync_style(&dark, &Metrics::BODY);
        let cached = view.flat(0, || {
            flatten_plain("a", SANS_FAMILY, FontWeight::NORMAL, dark.text)
        });

        view.sync_style(&dark, &Metrics::BODY);
        assert!(
            Rc::ptr_eq(
                &cached,
                &view.flat(0, || panic!("an unchanged style must reuse the cache"))
            ),
            "re-syncing the same style must not invalidate"
        );

        view.sync_style(&light, &Metrics::BODY);
        let relit = view.flat(0, || {
            flatten_plain("a", SANS_FAMILY, FontWeight::NORMAL, light.text)
        });
        assert!(!Rc::ptr_eq(&cached, &relit));
        assert_eq!(relit.runs[0].color, light.text);
    }

    /// Reasoning text goes through the same view as a response, so a plain
    /// prose block must actually produce renderable blocks.
    #[test]
    fn a_view_over_plain_prose_yields_blocks() {
        let mut view = MarkdownView::new();
        view.set_text("Let me check the parser first.", false);
        assert_eq!(view.blocks().count(), 1);

        // Streaming (mended) content too.
        let mut streaming = MarkdownView::new();
        streaming.set_text("Let me check the **parser", true);
        assert_eq!(streaming.blocks().count(), 1);

        // Empty content has nothing to render, which is the only case where
        // the renderer legitimately produces no element.
        let mut empty = MarkdownView::new();
        empty.set_text("", false);
        assert_eq!(empty.blocks().count(), 0);
    }

    #[test]
    fn markdown_view_blocks_swap_in_the_mended_tail() {
        let mut view = MarkdownView::new();
        view.set_text("Settled.\n\nNow **bold", true);
        let bold = view.blocks().any(|block| match block {
            Block::Paragraph { runs } => runs.iter().any(|run| run.style.bold),
            _ => false,
        });
        assert!(bold, "streaming emphasis should be styled");

        // Settled rendering keeps the markers literal.
        view.set_text("Settled.\n\nNow **bold", false);
        let bold = view.blocks().any(|block| match block {
            Block::Paragraph { runs } => runs.iter().any(|run| run.style.bold),
            _ => false,
        });
        assert!(!bold, "a settled response must not invent a closer");
        assert_eq!(view.blocks().count(), 2);
    }

    #[test]
    fn column_widths_are_content_proportional_and_floored() {
        let header = vec![runs_of("id"), runs_of("a much longer description column")];
        let widths = column_widths(&header, &[], 2);
        assert!(widths[1] > widths[0], "wider content gets a wider column");
        // The floor survives the fill, and the fractions still sum to one.
        let floor = 0.55 / 2.0;
        assert!(
            widths.iter().all(|width| *width >= floor - 1e-6),
            "every column keeps its floor: {widths:?}"
        );
        assert!((widths.iter().sum::<f32>() - 1.0).abs() < 1e-4);

        // A column that is merely narrow, not starved, stays proportional.
        let balanced = column_widths(&[runs_of("aaaa"), runs_of("bbbbbb")], &[], 2);
        assert!((balanced[0] - 0.4).abs() < 1e-3, "{balanced:?}");

        // An empty table falls back to even columns.
        let even = column_widths(&[], &[], 3);
        assert!(even.iter().all(|width| (width - 1.0 / 3.0).abs() < 1e-6));
    }
}

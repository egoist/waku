//! One measured element for a paragraph containing math. Word shaping and line
//! placement are retained across frames; formulas are atomic baseline-aligned
//! runs. Plain paragraphs continue to use GPUI's ordinary StyledText fast path.

use super::*;
use crate::md::math;
use crate::ui::menu::MenuItem;
use gpui::{
    App, AvailableSpace, Element, ElementId, GlobalElementId, InspectorElementId, LayoutId,
    ShapedLine, Size, TextAlign,
};
use std::sync::Arc;
use unicode_segmentation::UnicodeSegmentation;

#[derive(Debug)]
pub struct MathSpan {
    pub range: Range<usize>,
    pub latex: Arc<str>,
    pub display: bool,
}

#[derive(Debug)]
pub struct MathData {
    spans: Vec<MathSpan>,
    cache: RefCell<LayoutCache>,
    active_link: Rc<Cell<Option<usize>>>,
}

impl MathData {
    pub fn new(spans: Vec<MathSpan>) -> Self {
        Self {
            spans,
            cache: RefCell::new(LayoutCache::default()),
            active_link: Rc::new(Cell::new(None)),
        }
    }
}

#[derive(Default, Debug)]
struct LayoutCache {
    style: Option<(f32, f32, f32)>,
    keys: Vec<math::Key>,
    metrics: Vec<Option<math::Metrics>>,
    tokens: Vec<Token>,
    flow: Option<(f32, TextAlign, Rc<Flow>)>,
    intrinsic_flow: Option<Rc<Flow>>,
}

impl LayoutCache {
    fn flow(&mut self, width: f32, line_height: f32, alignment: TextAlign) -> Rc<Flow> {
        // Shrink-wrapped bubbles probe intrinsic width before laying out at
        // their final width. Keep both so those probes cannot thrash the cache.
        if !width.is_finite() {
            return self
                .intrinsic_flow
                .get_or_insert_with(|| {
                    Rc::new(arrange(&self.tokens, width, line_height, alignment))
                })
                .clone();
        }
        if self
            .flow
            .as_ref()
            .is_none_or(|(previous_width, previous_align, _)| {
                *previous_width != width || *previous_align != alignment
            })
        {
            self.flow = Some((
                width,
                alignment,
                Rc::new(arrange(&self.tokens, width, line_height, alignment)),
            ));
        }
        self.flow.as_ref().unwrap().2.clone()
    }
}

#[derive(Clone, Debug)]
enum Content {
    Text(ShapedLine),
    Formula(usize),
    Break,
}

#[derive(Clone, Debug)]
struct Token {
    range: Range<usize>,
    content: Content,
    width: f32,
    ascent: f32,
    descent: f32,
}

#[derive(Debug)]
struct Fragment {
    token: Token,
    origin: Point<Pixels>,
    row_top: f32,
    row_height: f32,
}

#[derive(Debug, Default)]
struct Flow {
    fragments: Vec<Fragment>,
    size: Size<Pixels>,
}

#[derive(Clone, Default)]
pub struct Geometry(Rc<RefCell<Option<(Bounds<Pixels>, Rc<Flow>)>>>);

impl Geometry {
    pub fn is_missing(&self) -> bool {
        self.0.borrow().is_none()
    }

    pub fn bounds(&self) -> Bounds<Pixels> {
        self.0
            .borrow()
            .as_ref()
            .map_or_else(Bounds::default, |(bounds, _)| *bounds)
    }

    fn formula_at(&self, position: Point<Pixels>) -> Option<usize> {
        let frame = self.0.borrow();
        let (bounds, flow) = frame.as_ref()?;
        if !bounds.contains(&position) {
            return None;
        }
        flow.fragments.iter().find_map(|fragment| {
            let Content::Formula(index) = fragment.token.content else {
                return None;
            };
            Bounds::new(
                bounds.origin + fragment.origin,
                size(
                    px(fragment.token.width),
                    px(fragment.token.ascent + fragment.token.descent),
                ),
            )
            .contains(&position)
            .then_some(index)
        })
    }

    fn formula_indexes(&self) -> Vec<usize> {
        self.0.borrow().as_ref().map_or_else(Vec::new, |(_, flow)| {
            flow.fragments
                .iter()
                .filter_map(|fragment| match fragment.token.content {
                    Content::Formula(index) => Some(index),
                    _ => None,
                })
                .collect()
        })
    }

    pub fn index_for_position(&self, position: Point<Pixels>) -> Result<usize, usize> {
        let frame = self.0.borrow();
        let Some((bounds, flow)) = frame.as_ref() else {
            return Err(0);
        };
        let local = position - bounds.origin;
        let mut best = None;
        for fragment in &flow.fragments {
            let x = f32::from(local.x - fragment.origin.x);
            let y = f32::from(local.y);
            let dy = (fragment.row_top - y).max(0.0)
                + (y - fragment.row_top - fragment.row_height).max(0.0);
            let dx = (-x).max(0.0) + (x - fragment.token.width).max(0.0);
            let distance = (dy, dx);
            if best
                .as_ref()
                .is_none_or(|(previous, _)| distance < *previous)
            {
                let range = &fragment.token.range;
                let offset = match &fragment.token.content {
                    Content::Text(line) => range.start + line.closest_index_for_x(px(x)),
                    Content::Formula(_) | Content::Break => {
                        if x < fragment.token.width / 2.0 {
                            range.start
                        } else {
                            range.end
                        }
                    }
                };
                best = Some((distance, offset));
            }
        }
        best.map_or(Err(0), |(_, index)| Ok(index))
    }

    pub fn range_rects(&self, range: &Range<usize>) -> Vec<Bounds<Pixels>> {
        let frame = self.0.borrow();
        let Some((bounds, flow)) = frame.as_ref() else {
            return Vec::new();
        };
        let mut rects: Vec<Bounds<Pixels>> = Vec::new();
        for fragment in &flow.fragments {
            let start = range.start.max(fragment.token.range.start);
            let end = range.end.min(fragment.token.range.end);
            if start >= end {
                continue;
            }
            let (left, right) = match &fragment.token.content {
                Content::Text(line) => (
                    line.x_for_index(start - fragment.token.range.start),
                    line.x_for_index(end - fragment.token.range.start),
                ),
                _ => (px(0.0), px(fragment.token.width)),
            };
            let rect = Bounds::new(
                bounds.origin + point(fragment.origin.x + left, px(fragment.row_top)),
                size(right - left, px(fragment.row_height)),
            );
            if let Some(last) = rects.last_mut()
                && last.top() == rect.top()
                && (f32::from(last.right() - rect.left())).abs() < 0.1
            {
                last.size.width = rect.right() - last.left();
            } else {
                rects.push(rect);
            }
        }
        rects
    }
}

pub(super) fn element(flat: Rc<FlatText>, key: TextKey, ctx: &Ctx) -> AnyElement {
    let geometry = Geometry::default();
    let mut wrapper = div()
        .relative()
        .w_full()
        .min_w_0()
        .cursor(CursorStyle::IBeam);
    if !flat.links.is_empty() {
        // Keep link activation in source coordinates, exactly like selection.
        // A focused paragraph cycles its links with arrows and opens with Enter.
        let focused_link = flat.math.as_ref().unwrap().active_link.clone();
        let links = flat.links.clone();
        let handler = ctx.link_handler.clone();
        let hit = geometry.clone();
        let selection = ctx.selection.clone();
        wrapper = wrapper.on_mouse_up(MouseButton::Left, move |event, window, cx| {
            if !hit.bounds().contains(&event.position) || !selection.selection.borrow().is_empty() {
                return;
            }
            let index = hit
                .index_for_position(event.position)
                .unwrap_or_else(|index| index);
            if let Some((_, url)) = links.iter().find(|(range, _)| {
                range.contains(&index)
                    && hit
                        .range_rects(range)
                        .iter()
                        .any(|rect| rect.contains(&event.position))
            }) {
                if let Some(handler) = &handler {
                    handler(url, window, cx);
                } else {
                    cx.open_url(url);
                }
            }
        });
        let links = flat.links.clone();
        let handler = ctx.link_handler.clone();
        wrapper = wrapper.on_key_down(move |event, window, cx| {
            let current = focused_link.get().unwrap_or(0).min(links.len() - 1);
            match event.keystroke.key.as_str() {
                "right" | "down" => focused_link.set(Some((current + 1) % links.len())),
                "left" | "up" => focused_link.set(Some((current + links.len() - 1) % links.len())),
                "home" => focused_link.set(Some(0)),
                "end" => focused_link.set(Some(links.len() - 1)),
                "escape" => focused_link.set(None),
                "enter" | "space" => {
                    let url = &links[current].1;
                    if let Some(handler) = &handler {
                        handler(url, window, cx);
                    } else {
                        cx.open_url(url);
                    }
                }
                _ => return,
            }
            cx.stop_propagation();
            cx.notify(window.current_view());
        });
    }
    let has_links = !flat.links.is_empty();
    let has_menu = ctx.math_menu.is_some();
    if let Some(menu) = &ctx.math_menu {
        let data = flat.math.as_ref().unwrap().clone();
        let hit = geometry.clone();
        let menu = menu.clone();
        wrapper = wrapper.on_mouse_down(MouseButton::Right, move |event, _, _| {
            if let Some(index) = hit.formula_at(event.position) {
                menu.set_context_items(vec![copy_expression_item(
                    data.spans[index].latex.clone(),
                    tr!("common.copy_expression"),
                )]);
            }
            // Bubble to the Markdown/message wrapper so its usual actions
            // remain in the same menu.
        });
        let data = flat.math.as_ref().unwrap().clone();
        let hit = geometry.clone();
        let menu = ctx.math_menu.as_ref().unwrap().clone();
        wrapper = wrapper.on_key_down(move |event, window, cx| {
            if event.keystroke.key != "contextmenu"
                && !(event.keystroke.key == "f10" && event.keystroke.modifiers.shift)
            {
                return;
            }
            let indexes = hit.formula_indexes();
            if indexes.is_empty() {
                return;
            }
            let items = if indexes.len() == 1 {
                vec![copy_expression_item(
                    data.spans[indexes[0]].latex.clone(),
                    tr!("common.copy_expression"),
                )]
            } else {
                // Keyboard invocation can choose every expression in this
                // paragraph, without relying on the pointer's position.
                std::iter::once(MenuItem::Header(tr!("common.copy_expression").into()))
                    .chain(indexes.into_iter().map(|index| {
                        let latex = data.spans[index].latex.clone();
                        copy_expression_item(latex.clone(), latex.to_string())
                    }))
                    .collect()
            };
            menu.set_context_items(items);
            menu.open_context_menu(window, cx);
            cx.stop_propagation();
            window.prevent_default();
        });
    }
    let body = MathText {
        flat,
        key: key.clone(),
        geometry,
        selection: ctx.selection.clone(),
        search: ctx.search.clone(),
        palette: *ctx.palette,
        block_break: ctx.take_block_break(),
    };
    wrapper
        .id(SharedString::from(format!(
            "math-{}-{}",
            key.row, key.index
        )))
        .when(has_links || has_menu, |element| {
            element
                .tab_group()
                .tab_index(0)
                .when(has_links, |element| {
                    element.border_1().border_color(gpui::transparent_black())
                })
                .focus_visible(|style| {
                    if has_links {
                        style.border_color(ctx.palette.accent)
                    } else {
                        style.bg(ctx.palette.overlay)
                    }
                })
        })
        .child(body)
        .into_any_element()
}

fn copy_expression_item(latex: Arc<str>, label: String) -> MenuItem {
    MenuItem::new(label, move |_, cx| {
        cx.write_to_clipboard(ClipboardItem::new_string(latex.to_string()));
    })
}

struct MathText {
    flat: Rc<FlatText>,
    key: TextKey,
    geometry: Geometry,
    selection: TranscriptSelection,
    search: Option<SearchHighlights>,
    palette: Palette,
    block_break: bool,
}

impl IntoElement for MathText {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for MathText {
    type RequestLayoutState = Vec<Option<Arc<math::Rendered>>>;
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }
    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let style = window.text_style();
        let font_size = f32::from(style.font_size.to_pixels(window.rem_size()));
        let line_height = f32::from(
            style
                .line_height
                .to_pixels(px(font_size).into(), window.rem_size()),
        );
        let scale = window.scale_factor();
        let data = self.flat.math.as_ref().unwrap();
        let mut cache = data.cache.borrow_mut();
        if cache.style != Some((font_size, line_height, scale)) {
            cache.style = Some((font_size, line_height, scale));
            cache.keys = data
                .spans
                .iter()
                .map(|span| {
                    math::Key::new(
                        span.latex.clone(),
                        span.display,
                        font_size,
                        scale,
                        self.palette.text,
                    )
                })
                .collect();
            cache.tokens.clear();
            cache.flow = None;
            cache.intrinsic_flow = None;
        }
        let images = math::request(&cache.keys, window.current_view(), cx);
        let metrics = images
            .iter()
            .map(|image| image.as_ref().map(|image| image.metrics))
            .collect::<Vec<_>>();
        if cache.tokens.is_empty() || cache.metrics != metrics {
            cache.tokens = tokens(
                &self.flat,
                &data.spans,
                &metrics,
                font_size,
                line_height,
                window,
            );
            cache.metrics = metrics;
            cache.flow = None;
            cache.intrinsic_flow = None;
        }
        drop(cache);
        let flat = self.flat.clone();
        let geometry = self.geometry.clone();
        let alignment = if data.spans.len() == 1 && data.spans[0].display {
            TextAlign::Center
        } else {
            style.text_align
        };
        let id =
            window.request_measured_layout(Default::default(), move |known, available, _, _| {
                let width = known
                    .width
                    .or(match available.width {
                        AvailableSpace::Definite(width) => Some(width),
                        _ => None,
                    })
                    .map_or(f32::INFINITY, |width| f32::from(width).max(1.0));
                let mut cache = flat.math.as_ref().unwrap().cache.borrow_mut();
                let flow = cache.flow(width, line_height, alignment);
                let size = flow.size;
                *geometry.0.borrow_mut() = Some((Bounds::new(point(px(0.0), px(0.0)), size), flow));
                size
            });
        (id, images)
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        _: &mut Window,
        _: &mut App,
    ) {
        if let Some((stored, _)) = self.geometry.0.borrow_mut().as_mut() {
            *stored = bounds;
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        images: &mut Self::RequestLayoutState,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let wash = |rect: Bounds<Pixels>, color: Hsla, radius: f32, window: &mut Window| {
            window.paint_quad(quad(
                rect,
                px(radius),
                color,
                px(0.0),
                gpui::transparent_black(),
                BorderStyle::default(),
            ));
        };
        for range in &self.flat.code_ranges {
            for rect in self.geometry.range_rects(range) {
                wash(rect, self.palette.code_wash, CODE_WASH_RADIUS, window);
            }
        }
        if let Some(search) = &self.search {
            for found in search
                .matches
                .iter()
                .filter(|found| found.ordinal == self.key.index)
            {
                let color = if search.active.as_ref() == Some(found) {
                    self.palette.active_search_match
                } else {
                    self.palette.search_match
                };
                for rect in self.geometry.range_rects(&found.range) {
                    wash(rect, color, 2.0, window);
                }
            }
        }
        if let Some(range) = self.selection.selection.borrow().wash_range(&self.key) {
            for rect in self.geometry.range_rects(&range) {
                wash(rect, self.palette.selection, 0.0, window);
            }
        }
        if let Some(index) = self.flat.math.as_ref().unwrap().active_link.get()
            && let Some((range, _)) = self.flat.links.get(index)
        {
            for rect in self.geometry.range_rects(range) {
                window.paint_quad(quad(
                    rect,
                    px(2.0),
                    gpui::transparent_black(),
                    px(1.0),
                    self.palette.accent,
                    BorderStyle::default(),
                ));
            }
        }
        if let Some((_, flow)) = self.geometry.0.borrow().as_ref() {
            for fragment in &flow.fragments {
                let origin = bounds.origin + fragment.origin;
                match &fragment.token.content {
                    Content::Text(line) => {
                        let _ = line.paint(
                            origin,
                            px(fragment.token.ascent + fragment.token.descent),
                            TextAlign::Left,
                            None,
                            window,
                            cx,
                        );
                    }
                    Content::Formula(index) => {
                        if let Some(image) = &images[*index] {
                            let image_bounds = Bounds::new(
                                origin,
                                size(
                                    px(fragment.token.width),
                                    px(fragment.token.ascent + fragment.token.descent),
                                ),
                            );
                            let _ = window.paint_image(
                                bounds,
                                image_bounds,
                                Default::default(),
                                image.image.clone(),
                                0,
                                false,
                            );
                        }
                    }
                    Content::Break => {}
                }
            }
        }
        self.selection.registry.borrow_mut().push(RegisteredText {
            key: self.key.clone(),
            text: Rc::from(self.flat.text.as_ref()),
            block_break: self.block_break,
            geometry: TextGeometry::Math(self.geometry.clone()),
        });
    }
}

fn slice_runs(runs: &[TextRun], range: Range<usize>) -> Vec<TextRun> {
    let mut start = 0;
    runs.iter()
        .filter_map(|run| {
            let end = start + run.len;
            let len = end.min(range.end).saturating_sub(start.max(range.start));
            start = end;
            (len > 0).then(|| TextRun { len, ..run.clone() })
        })
        .collect()
}

fn tokens(
    flat: &FlatText,
    spans: &[MathSpan],
    metrics: &[Option<math::Metrics>],
    font_size: f32,
    line_height: f32,
    window: &mut Window,
) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut offset = 0;
    for (index, span) in spans.iter().enumerate() {
        text_tokens(
            &mut tokens,
            flat,
            offset..span.range.start,
            font_size,
            line_height,
            window,
        );
        if let Some(metrics) = metrics[index] {
            tokens.push(Token {
                range: span.range.clone(),
                content: Content::Formula(index),
                width: metrics.width,
                ascent: metrics.ascent,
                descent: metrics.descent,
            });
        } else {
            text_tokens(
                &mut tokens,
                flat,
                span.range.clone(),
                font_size,
                line_height,
                window,
            );
        }
        offset = span.range.end;
    }
    text_tokens(
        &mut tokens,
        flat,
        offset..flat.text.len(),
        font_size,
        line_height,
        window,
    );
    tokens
}

fn text_tokens(
    tokens: &mut Vec<Token>,
    flat: &FlatText,
    range: Range<usize>,
    font_size: f32,
    line_height: f32,
    window: &mut Window,
) {
    let mut offset = 0;
    for (end, _) in unicode_linebreak::linebreaks(&flat.text[range.clone()]) {
        let word = &flat.text[range.start + offset..range.start + end];
        // Unicode word boundaries may group a run of spaces with a newline.
        let mut start = range.start + offset;
        for part in word.split_inclusive('\n') {
            let text = part.trim_end_matches('\n');
            if !text.is_empty() {
                let end = start + text.len();
                let runs = slice_runs(&flat.runs, start..end);
                let line = window.text_system().shape_line(
                    SharedString::from(text.to_owned()),
                    px(font_size),
                    &runs,
                    None,
                );
                let leading = (line_height - f32::from(line.ascent + line.descent)) / 2.0;
                tokens.push(Token {
                    range: start..end,
                    width: f32::from(line.width()),
                    ascent: f32::from(line.ascent) + leading,
                    descent: f32::from(line.descent) + leading,
                    content: Content::Text(line),
                });
                start = end;
            }
            if part.ends_with('\n') {
                tokens.push(Token {
                    range: start..start + 1,
                    content: Content::Break,
                    width: 0.0,
                    ascent: line_height * 0.8,
                    descent: line_height * 0.2,
                });
                start += 1;
            }
        }
        offset = end;
    }
}

fn arrange(tokens: &[Token], width: f32, line_height: f32, align: TextAlign) -> Flow {
    let mut flow = Flow::default();
    let mut row_start = 0;
    let mut x = 0.0;
    let mut y = 0.0;
    let mut ascent = line_height * 0.8;
    let mut descent = line_height * 0.2;
    let mut max_width = 0.0f32;
    let finish_row = |flow: &mut Flow, start: usize, x: f32, y: f32, ascent: f32, descent: f32| {
        let inset = if width.is_finite() {
            match align {
                TextAlign::Center => (width - x).max(0.0) / 2.0,
                TextAlign::Right => (width - x).max(0.0),
                _ => 0.0,
            }
        } else {
            0.0
        };
        for fragment in &mut flow.fragments[start..] {
            fragment.origin.x += px(inset);
            fragment.origin.y = px(y + ascent - fragment.token.ascent);
            fragment.row_top = y;
            fragment.row_height = ascent + descent;
        }
    };
    for original in tokens {
        let mut token = original.clone();
        if matches!(token.content, Content::Formula(_)) && token.width > width {
            let scale = width / token.width;
            token.width = width;
            token.ascent *= scale;
            token.descent *= scale;
        }
        let mut remainder = Some(token);
        while let Some(mut token) = remainder.take() {
            let hard_break = matches!(token.content, Content::Break);
            if hard_break || (x > 0.0 && x + token.width > width) {
                finish_row(&mut flow, row_start, x, y, ascent, descent);
                max_width = max_width.max(x);
                y += ascent + descent;
                row_start = flow.fragments.len();
                x = 0.0;
                ascent = line_height * 0.8;
                descent = line_height * 0.2;
            }
            if hard_break {
                continue;
            }
            // Very long words/URLs still wrap at grapheme boundaries. This
            // only splits cached glyph data; it never calls the shaper again.
            if token.width > width
                && let Content::Text(line) = &token.content
            {
                let boundary = line
                    .text
                    .grapheme_indices(true)
                    .map(|(i, text)| i + text.len())
                    .take_while(|end| line.x_for_index(*end) <= px(width))
                    .last()
                    .unwrap_or_else(|| line.text.graphemes(true).next().map_or(0, str::len));
                if boundary > 0 && boundary < line.len() {
                    let (left, right) = line.split_at(boundary);
                    let middle = token.range.start + boundary;
                    remainder = Some(Token {
                        range: middle..token.range.end,
                        width: f32::from(right.width()),
                        content: Content::Text(right),
                        ..token.clone()
                    });
                    token.range.end = middle;
                    token.width = f32::from(left.width());
                    token.content = Content::Text(left);
                }
            }
            ascent = ascent.max(token.ascent);
            descent = descent.max(token.descent);
            let token_width = token.width;
            flow.fragments.push(Fragment {
                token,
                origin: point(px(x), px(y)),
                row_top: y,
                row_height: 0.0,
            });
            x += token_width;
        }
    }
    finish_row(&mut flow, row_start, x, y, ascent, descent);
    max_width = max_width.max(x);
    flow.size = size(
        px(if width.is_finite() { width } else { max_width }),
        px(y + ascent + descent),
    );
    flow
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MenuHarness {
        markdown: MarkdownView,
        selection: TranscriptSelection,
        menu: ContextMenuHandle,
        enabled: bool,
    }

    impl gpui::Render for MenuHarness {
        fn render(&mut self, _: &mut Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
            let palette = Palette::from_theme(&Theme::dark());
            let ctx = Ctx::new(
                "math-menu-test",
                &palette,
                Metrics::BODY,
                self.selection.clone(),
            )
            .with_math_enabled(self.enabled)
            .with_context_menu(self.menu.clone());
            context_menu(
                div()
                    .w(px(500.0))
                    .child(frame_reset(self.selection.clone()))
                    .children(markdown(&self.markdown, &ctx)),
                "math-message-menu",
                &self.menu,
                |_| {
                    vec![MenuItem::new("Copy Message", |_, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(
                            "whole message".to_owned(),
                        ));
                    })]
                },
            )
        }
    }

    #[gpui::test]
    fn disabling_math_skips_render_requests_and_toggles_existing_content(
        cx: &mut gpui::TestAppContext,
    ) {
        let mut markdown = MarkdownView::new();
        markdown.set_text(r"Before $x^2$ after $$\frac{a}{b}$$ done", false);
        let menu = cx.update(ContextMenuHandle::new);
        let (view, cx) = cx.add_window_view(|_, _| MenuHarness {
            markdown,
            menu,
            selection: TranscriptSelection::default(),
            enabled: false,
        });
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert!(
                view.selection
                    .registry
                    .borrow()
                    .entries()
                    .iter()
                    .all(|entry| matches!(entry.geometry, TextGeometry::Text(_)))
            );
            for data in view
                .markdown
                .flats
                .borrow()
                .values()
                .filter_map(|flat| flat.math.as_ref())
            {
                assert!(
                    data.cache.borrow().keys.is_empty(),
                    "disabled math must not queue native work"
                );
            }
        });
        view.update(cx, |view, cx| {
            view.enabled = true;
            cx.notify();
        });
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert_eq!(
                view.selection
                    .registry
                    .borrow()
                    .entries()
                    .iter()
                    .filter(|entry| matches!(entry.geometry, TextGeometry::Math(_)))
                    .count(),
                2
            );
        });
        view.update(cx, |view, cx| {
            view.enabled = false;
            cx.notify();
        });
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert!(
                view.selection
                    .registry
                    .borrow()
                    .entries()
                    .iter()
                    .all(|entry| matches!(entry.geometry, TextGeometry::Text(_)))
            );
        });
    }

    #[gpui::test]
    fn formula_context_actions_copy_the_clicked_source_and_preserve_message_actions(
        cx: &mut gpui::TestAppContext,
    ) {
        let mut markdown = MarkdownView::new();
        markdown.set_text(r"Before $x^2$ then $\frac{1}{2}$ after", false);
        let menu = cx.update(ContextMenuHandle::new);
        let (view, cx) = cx.add_window_view(|_, _| MenuHarness {
            markdown,
            menu: menu.clone(),
            selection: TranscriptSelection::default(),
            enabled: true,
        });
        cx.run_until_parked();
        let formula_point = view.read_with(cx, |view, _| {
            let registry = view.selection.registry.borrow();
            let TextGeometry::Math(geometry) = &registry.entries()[0].geometry else {
                panic!()
            };
            let frame = geometry.0.borrow();
            let (bounds, flow) = frame.as_ref().unwrap();
            let fragment = flow
                .fragments
                .iter()
                .find(|fragment| matches!(fragment.token.content, Content::Formula(1)))
                .unwrap();
            bounds.origin + fragment.origin + point(px(fragment.token.width / 2.0), px(2.0))
        });
        cx.simulate_mouse_down(formula_point, MouseButton::Right, gpui::Modifiers::none());
        assert!(menu.is_open());
        cx.update(|window, cx| window.focus(menu.focus_handle(), cx));
        cx.simulate_keystrokes("down enter");
        cx.update(|_, cx| {
            assert_eq!(
                cx.read_from_clipboard().unwrap().text().unwrap(),
                r"\frac{1}{2}"
            )
        });

        // The ordinary message action remains after the injected expression.
        cx.simulate_mouse_down(formula_point, MouseButton::Right, gpui::Modifiers::none());
        cx.update(|window, cx| window.focus(menu.focus_handle(), cx));
        cx.simulate_keystrokes("down down enter");
        cx.update(|_, cx| {
            assert_eq!(
                cx.read_from_clipboard().unwrap().text().unwrap(),
                "whole message"
            )
        });

        // The following prose click must not retain the previous formula item.
        cx.simulate_mouse_down(
            point(px(2.0), px(2.0)),
            MouseButton::Right,
            gpui::Modifiers::none(),
        );
        cx.update(|window, cx| window.focus(menu.focus_handle(), cx));
        cx.simulate_keystrokes("down enter");
        cx.update(|_, cx| {
            assert_eq!(
                cx.read_from_clipboard().unwrap().text().unwrap(),
                "whole message"
            )
        });

        // The opened action is a source snapshot even if the message changes.
        cx.simulate_mouse_down(formula_point, MouseButton::Right, gpui::Modifiers::none());
        view.update(cx, |view, cx| {
            view.markdown.set_text("New $z$", false);
            cx.notify();
        });
        cx.run_until_parked();
        cx.update(|window, cx| window.focus(menu.focus_handle(), cx));
        cx.simulate_keystrokes("down enter");
        cx.update(|_, cx| {
            assert_eq!(
                cx.read_from_clipboard().unwrap().text().unwrap(),
                r"\frac{1}{2}"
            )
        });
    }

    fn formula(range: Range<usize>, width: f32, ascent: f32, descent: f32) -> Token {
        Token {
            range,
            width,
            ascent,
            descent,
            content: Content::Formula(0),
        }
    }

    #[test]
    fn formulas_wrap_atomically_align_baselines_and_scale_to_narrow_widths() {
        let tokens = vec![
            formula(0..3, 40.0, 10.0, 3.0),
            formula(3..9, 50.0, 25.0, 8.0),
        ];
        let flow = arrange(&tokens, 100.0, 21.0, TextAlign::Left);
        assert_eq!(
            flow.fragments[0].origin.y + px(10.0),
            flow.fragments[1].origin.y + px(25.0)
        );
        let wrapped = arrange(&tokens, 70.0, 21.0, TextAlign::Left);
        assert!(wrapped.fragments[1].row_top >= wrapped.fragments[0].row_height);
        let narrow = arrange(&tokens[1..], 20.0, 21.0, TextAlign::Center);
        assert_eq!(narrow.size.width, px(20.0));
        assert_eq!(narrow.fragments[0].token.width, 20.0);
        assert_eq!(narrow.fragments[0].token.ascent, 10.0);
    }

    #[test]
    fn display_math_is_centered_inside_its_measured_bounds() {
        let flow = arrange(
            &[formula(0..5, 80.0, 20.0, 5.0)],
            400.0,
            21.0,
            TextAlign::Center,
        );
        assert_eq!(flow.fragments[0].origin.x, px(160.0));
        assert_eq!(flow.size.width, px(400.0));
        assert!(flow.fragments[0].origin.x + px(80.0) <= flow.size.width);
    }

    #[test]
    fn settled_math_reuses_placed_glyphs_and_geometry_without_retaining_images() {
        let mut cache = LayoutCache {
            tokens: vec![formula(0..5, 80.0, 20.0, 5.0)],
            ..Default::default()
        };
        let first = cache.flow(400.0, 21.0, TextAlign::Left);
        for _ in 0..1000 {
            cache.flow(f32::INFINITY, 21.0, TextAlign::Left);
            assert!(Rc::ptr_eq(
                &first,
                &cache.flow(400.0, 21.0, TextAlign::Left)
            ));
        }
        assert!(!Rc::ptr_eq(
            &first,
            &cache.flow(100.0, 21.0, TextAlign::Left)
        ));
    }

    #[test]
    fn formula_selection_and_search_map_to_latex_source() {
        let flow = Rc::new(arrange(
            &[formula(4..18, 80.0, 20.0, 5.0)],
            100.0,
            21.0,
            TextAlign::Left,
        ));
        let geometry = Geometry::default();
        *geometry.0.borrow_mut() = Some((Bounds::new(point(px(10.0), px(20.0)), flow.size), flow));
        assert_eq!(
            geometry.index_for_position(point(px(11.0), px(25.0))),
            Ok(4)
        );
        assert_eq!(
            geometry.index_for_position(point(px(89.0), px(25.0))),
            Ok(18)
        );
        let rects = geometry.range_rects(&(7..10));
        assert_eq!(rects.len(), 1);
        assert_eq!(rects[0].left(), px(10.0));
        assert_eq!(rects[0].size.width, px(80.0));
    }

    #[gpui::test]
    fn mixed_text_preserves_styles_unicode_and_source_offsets(cx: &mut gpui::TestAppContext) {
        struct TestWindow;
        impl gpui::Render for TestWindow {
            fn render(&mut self, _: &mut Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
                div()
            }
        }
        let (_, cx) = cx.add_window_view(|_, _| TestWindow);
        cx.update(|window, _| {
            let source = "before **bold** $x^2$ 中文 👨‍👩‍👧‍👦 after";
            let tree = crate::md::parser::parse(source);
            let Block::Paragraph { runs } = &tree.blocks[0].block else {
                panic!()
            };
            let palette = Palette::from_theme(&Theme::dark());
            let flat = flatten(runs, &palette, FontWeight::NORMAL, palette.text);
            let data = flat.math.as_ref().unwrap();
            let metrics = [Some(math::Metrics {
                width: 30.0,
                ascent: 15.0,
                descent: 3.0,
            })];
            let tokens = tokens(&flat, &data.spans, &metrics, 14.0, 21.0, window);
            assert_eq!(tokens.first().unwrap().range.start, 0);
            assert_eq!(tokens.last().unwrap().range.end, flat.text.len());
            assert!(
                tokens
                    .windows(2)
                    .all(|pair| pair[0].range.end == pair[1].range.start)
            );
            let flow = arrange(&tokens, 90.0, 21.0, TextAlign::Left);
            assert!(flow.size.height > px(21.0));
            assert!(flow.fragments.iter().all(|fragment| {
                flat.text.is_char_boundary(fragment.token.range.start)
                    && flat.text.is_char_boundary(fragment.token.range.end)
            }));
        });
    }

    #[gpui::test]
    fn background_math_completion_renders_and_then_reuses_the_same_layout(
        cx: &mut gpui::TestAppContext,
    ) {
        struct Harness {
            markdown: MarkdownView,
            selection: TranscriptSelection,
        }
        impl gpui::Render for Harness {
            fn render(&mut self, _: &mut Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
                let palette = Palette::from_theme(&Theme::dark());
                let ctx = Ctx::new("math-test", &palette, Metrics::BODY, self.selection.clone());
                div()
                    .w(px(400.0))
                    .child(frame_reset(self.selection.clone()))
                    .children(markdown(&self.markdown, &ctx))
            }
        }
        let mut markdown = MarkdownView::new();
        markdown.set_text(r"Before $x^2$ after $$\frac{a}{b}$$ done", false);
        let (view, cx) = cx.add_window_view(|_, _| Harness {
            markdown,
            selection: TranscriptSelection::default(),
        });
        cx.run_until_parked();
        let flows = view.read_with(cx, |view, _| {
            assert_eq!(view.selection.registry.borrow().entries().len(), 3);
            view.markdown
                .flats
                .borrow()
                .values()
                .filter_map(|flat| flat.math.as_ref())
                .map(|data| {
                    let cache = data.cache.borrow();
                    assert!(
                        cache.metrics.iter().all(Option::is_some),
                        "worker must complete native rendering"
                    );
                    cache.flow.as_ref().unwrap().2.clone()
                })
                .collect::<Vec<_>>()
        });
        assert_eq!(flows.len(), 2);
        view.update(cx, |_, cx| cx.notify());
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            let current = view.markdown.flats.borrow();
            let current = current
                .values()
                .filter_map(|flat| flat.math.as_ref())
                .map(|data| data.cache.borrow().flow.as_ref().unwrap().2.clone())
                .collect::<Vec<_>>();
            assert!(
                flows
                    .iter()
                    .zip(current)
                    .all(|(previous, current)| Rc::ptr_eq(previous, &current))
            );
        });
    }
}

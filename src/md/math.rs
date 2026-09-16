//! Bounded native math workers. Frames only look up immutable results; neither
//! the TeX engine nor SVG/font loading is ever called on the UI thread.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, LazyLock, Mutex};

use gpui::{App, EntityId, Global, Hsla, RenderImage, SvgRenderer};
use ratex_types::{color::Color, display_item::DisplayList, math_style::MathStyle};

const MAX_SOURCE_BYTES: usize = 8 * 1024;
const MAX_ENTRIES: usize = 256;
const MAX_CACHE_BYTES: usize = 32 * 1024 * 1024;
const MAX_PENDING: usize = 128;
const WORKERS: usize = 2;
const BATCH_SIZE: usize = 8;
const MAX_PIXELS: f64 = 2_000_000.0;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(super) struct Key {
    pub latex: Arc<str>,
    pub display: bool,
    font_size: u32,
    scale: u32,
    color: [u8; 4],
}

impl Key {
    pub fn new(latex: Arc<str>, display: bool, font_size: f32, scale: f32, color: Hsla) -> Self {
        let color: gpui::Rgba = color.into();
        Self {
            latex,
            display,
            font_size: font_size.clamp(6.0, 96.0).to_bits(),
            scale: scale.clamp(1.0, 3.0).to_bits(),
            color: [color.r, color.g, color.b, color.a].map(|v| (v * 255.0).round() as u8),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Metrics {
    pub width: f32,
    pub ascent: f32,
    pub descent: f32,
}

pub(super) struct Rendered {
    pub metrics: Metrics,
    pub image: Arc<RenderImage>,
}

enum State {
    Pending(HashSet<EntityId>),
    // Failures are cached too, so malformed input does not retry every frame.
    Ready(Option<Arc<Rendered>>),
}

struct Entry {
    state: State,
    touched: u64,
    bytes: usize,
}

#[derive(Default)]
struct Cache {
    entries: HashMap<Key, Entry>,
    queue: VecDeque<Key>,
    waiting: HashSet<EntityId>,
    clock: u64,
    bytes: usize,
    active: usize,
    pending: usize,
    retired: Vec<Arc<RenderImage>>,
}

// Interior mutability lets cache hits avoid GPUI's global-observer effects.
// This store belongs to the UI thread and never takes a blocking lock.
#[derive(Default)]
struct Store(RefCell<Cache>);
impl Global for Store {}

impl Cache {
    fn request(&mut self, key: &Key, view: EntityId) -> Option<Arc<Rendered>> {
        self.clock += 1;
        if let Some(entry) = self.entries.get_mut(key) {
            entry.touched = self.clock;
            return match &mut entry.state {
                State::Pending(views) => {
                    views.insert(view);
                    None
                }
                State::Ready(result) => result.clone(),
            };
        }
        if self.pending >= MAX_PENDING {
            self.waiting.insert(view);
            return None;
        }
        self.entries.insert(
            key.clone(),
            Entry {
                state: State::Pending(HashSet::from([view])),
                touched: self.clock,
                bytes: 0,
            },
        );
        self.queue.push_back(key.clone());
        self.pending += 1;
        self.trim();
        None
    }

    fn trim(&mut self) {
        while self.entries.len() > MAX_ENTRIES || self.bytes > MAX_CACHE_BYTES {
            let oldest = self
                .entries
                .iter()
                .filter(|(_, entry)| matches!(entry.state, State::Ready(_)))
                .min_by_key(|(_, entry)| entry.touched)
                .map(|(key, _)| key.clone());
            let Some(key) = oldest else { break };
            let entry = self.entries.remove(&key).unwrap();
            self.bytes -= entry.bytes;
            if let State::Ready(Some(rendered)) = entry.state {
                self.retired.push(rendered.image.clone());
            }
        }
    }

    fn complete(&mut self, results: Vec<(Key, Option<Arc<Rendered>>)>) -> HashSet<EntityId> {
        let mut notify = std::mem::take(&mut self.waiting);
        for (key, result) in results {
            // Pending entries are never evicted or replaced. Content, size,
            // theme and display scale are part of the immutable key, so an old
            // completion cannot overwrite a newer formula/style.
            if let Some(entry) = self.entries.get_mut(&key) {
                let State::Pending(views) = &mut entry.state else {
                    continue;
                };
                notify.extend(views.drain());
                entry.bytes = result
                    .as_ref()
                    .map_or(0, |r| r.image.as_bytes(0).map_or(0, |b| b.len()));
                self.bytes += entry.bytes;
                entry.state = State::Ready(result);
                self.pending -= 1;
            }
        }
        self.trim();
        notify
    }
}

/// Queue a visible paragraph's formulas together, deduplicating across every
/// MarkdownView and notifying each observing pane once per completed batch.
pub(super) fn request(keys: &[Key], view: EntityId, cx: &mut App) -> Vec<Option<Arc<Rendered>>> {
    if !cx.has_global::<Store>() {
        cx.set_global(Store::default());
    }
    let results = {
        let mut cache = cx.global::<Store>().0.borrow_mut();
        keys.iter().map(|key| cache.request(key, view)).collect()
    };
    retire_images(cx);
    pump(cx);
    results
}

fn retire_images(cx: &mut App) {
    let images = std::mem::take(&mut cx.global::<Store>().0.borrow_mut().retired);
    if !images.is_empty() {
        // An Arc dropping does not remove GPUI's sprite-atlas entry. Do that
        // explicitly after the current frame, as GPUI's native image caches do.
        cx.defer(move |cx| {
            for image in images {
                cx.drop_image(image, None);
            }
        });
    }
}

fn pump(cx: &mut App) {
    loop {
        let batch = {
            let mut cache = cx.global::<Store>().0.borrow_mut();
            if cache.active >= WORKERS || cache.queue.is_empty() {
                return;
            }
            cache.active += 1;
            let count = BATCH_SIZE.min(cache.queue.len());
            cache.queue.drain(..count).collect::<Vec<_>>()
        };
        let renderer = cx.svg_renderer();
        let task = cx.background_executor().spawn(async move {
            batch
                .into_iter()
                .map(|key| {
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        render(&key, &renderer).ok().map(Arc::new)
                    }))
                    .ok()
                    .flatten();
                    (key, result)
                })
                .collect()
        });
        cx.spawn(async move |cx| {
            let results = task.await;
            let _ = cx.update(|cx| {
                let mut cache = cx.global::<Store>().0.borrow_mut();
                cache.active -= 1;
                let views = cache.complete(results);
                drop(cache);
                for view in views {
                    cx.notify(view);
                }
                retire_images(cx);
                pump(cx);
            });
        })
        .detach();
    }
}

// Color/scale-independent typesetting survives a theme or monitor change.
// This mutex is accessed ONLY by workers, never by a frame or UI callback.
type LayoutKey = (Arc<str>, bool);
#[derive(Default)]
struct LayoutCache {
    entries: HashMap<LayoutKey, Arc<DisplayList>>,
    order: VecDeque<LayoutKey>,
    items: usize,
}
static LAYOUTS: LazyLock<Mutex<LayoutCache>> = LazyLock::new(|| Mutex::new(LayoutCache::default()));

fn typeset(latex: &Arc<str>, display: bool) -> anyhow::Result<Arc<DisplayList>> {
    anyhow::ensure!(
        !latex.trim().is_empty() && latex.len() <= MAX_SOURCE_BYTES,
        "math source limit"
    );
    let key = (latex.clone(), display);
    if let Some(layout) = LAYOUTS.lock().unwrap().entries.get(&key).cloned() {
        return Ok(layout);
    }
    let ast = ratex_parser::parse(latex).map_err(|error| anyhow::anyhow!("{error:?}"))?;
    let options = ratex_layout::LayoutOptions {
        style: if display {
            MathStyle::Display
        } else {
            MathStyle::Text
        },
        ..Default::default()
    };
    let layout = ratex_layout::layout(&ast, &options);
    let list = Arc::new(ratex_layout::to_display_list(&layout));
    anyhow::ensure!(list.items.len() <= 8192, "math layout limit");
    let mut cache = LAYOUTS.lock().unwrap();
    if let Some(existing) = cache.entries.get(&key) {
        return Ok(existing.clone());
    }
    cache.items += list.items.len();
    cache.entries.insert(key.clone(), list.clone());
    cache.order.push_back(key);
    while cache.entries.len() > 128 || cache.items > 32768 {
        if let Some(key) = cache.order.pop_front() {
            if let Some(list) = cache.entries.remove(&key) {
                cache.items -= list.items.len();
            }
        }
    }
    Ok(list)
}

fn render(key: &Key, renderer: &SvgRenderer) -> anyhow::Result<Rendered> {
    let mut list = (*typeset(&key.latex, key.display)?).clone();
    let color = key.color.map(|v| v as f32 / 255.0);
    let color = Color::new(color[0], color[1], color[2], color[3]);
    for item in &mut list.items {
        use ratex_types::display_item::DisplayItem::*;
        let (GlyphPath { color: paint, .. }
        | Line { color: paint, .. }
        | Rect { color: paint, .. }
        | Path { color: paint, .. }) = item;
        // Preserve explicit TeX colors; the default ink follows the theme.
        if *paint == Color::BLACK {
            *paint = color;
        }
    }
    let font_size = f32::from_bits(key.font_size) as f64;
    let metrics = Metrics {
        width: (list.width * font_size + 2.0) as f32,
        ascent: (list.height * font_size + 1.0) as f32,
        descent: (list.depth * font_size + 1.0) as f32,
    };
    let width = metrics.width as f64;
    let height = (metrics.ascent + metrics.descent) as f64;
    // GPUI supersamples SVGs at 2x in addition to the display's scale factor.
    let raster_scale = f32::from_bits(key.scale) as f64 * 2.0;
    anyhow::ensure!(
        width.is_finite()
            && height.is_finite()
            && width > 0.0
            && height > 0.0
            && width * raster_scale <= 4096.0
            && height * raster_scale <= 4096.0
            && width * height * raster_scale * raster_scale <= MAX_PIXELS,
        "math image limit"
    );
    let svg = ratex_svg::render_to_svg_with_color_syntax(
        &list,
        &ratex_svg::SvgOptions {
            font_size,
            padding: 1.0,
            embed_glyphs: true,
            ..Default::default()
        },
        ratex_svg::SvgColorSyntax::Rgb,
    );
    // RaTeX labels its viewport in pt; our metrics are logical pixels.
    let svg = svg.replacen("pt\"", "\"", 2);
    let image = renderer.render_single_frame(svg.as_bytes(), f32::from_bits(key.scale))?;
    Ok(Rendered { metrics, image })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_typesetting_covers_common_formulas_and_reuses_layout() {
        for source in [
            r"E=mc^2",
            r"\frac{-b\pm\sqrt{b^2-4ac}}{2a}",
            r"\int_0^\infty e^{-x}\,dx=1",
            r"\begin{pmatrix}a&b\\c&d\end{pmatrix}",
        ] {
            let source: Arc<str> = source.into();
            let layout = typeset(&source, true).unwrap();
            assert!(layout.width > 0.0 && layout.height > 0.0 && !layout.items.is_empty());
            assert!(Arc::ptr_eq(&layout, &typeset(&source, true).unwrap()));
        }
    }

    #[test]
    fn invalid_and_oversized_input_fails_without_rasterizing() {
        assert!(typeset(&Arc::from(r"\notARealMathCommand{x}"), false).is_err());
        assert!(typeset(&Arc::from("x".repeat(MAX_SOURCE_BYTES + 1)), false).is_err());
        assert!(typeset(&Arc::from(""), false).is_err());
    }

    #[test]
    fn native_raster_is_visible_and_theme_changes_preserve_geometry() {
        let renderer = SvgRenderer::new(Arc::new(()));
        let key = Key::new(
            Arc::from(r"\frac{x^2+1}{\sqrt{y}}"),
            true,
            14.0,
            2.0,
            gpui::black(),
        );
        let dark = render(&key, &renderer).unwrap();
        let light = render(
            &Key {
                color: [240, 240, 240, 255],
                ..key.clone()
            },
            &renderer,
        )
        .unwrap();
        assert_eq!(dark.metrics, light.metrics);
        assert!(
            dark.image
                .as_bytes(0)
                .unwrap()
                .chunks_exact(4)
                .any(|pixel| pixel[3] > 0)
        );
        assert_ne!(dark.image.as_bytes(0), light.image.as_bytes(0));
        assert!(
            render(
                &Key::new(
                    Arc::from(r"\rule{10000em}{10000em}"),
                    true,
                    14.0,
                    2.0,
                    gpui::black()
                ),
                &renderer
            )
            .is_err()
        );
    }

    #[test]
    fn pending_work_is_deduplicated_bounded_and_failures_are_cached() {
        let mut cache = Cache::default();
        let key = Key::new(Arc::from("x"), false, 14.0, 2.0, gpui::black());
        let view = EntityId::from(1);
        for _ in 0..1000 {
            assert!(cache.request(&key, view).is_none());
        }
        assert_eq!(cache.queue.len(), 1);
        assert_eq!(cache.pending, 1);
        assert_eq!(
            cache.complete(vec![(key.clone(), None)]),
            HashSet::from([view])
        );
        cache.queue.clear();
        for _ in 0..1000 {
            assert!(cache.request(&key, view).is_none());
        }
        assert!(cache.queue.is_empty());
        for i in 0..MAX_PENDING * 2 {
            cache.request(
                &Key {
                    latex: Arc::from(format!("x_{i}")),
                    ..key.clone()
                },
                view,
            );
        }
        assert_eq!(cache.pending, MAX_PENDING);
        assert_eq!(cache.queue.len(), MAX_PENDING);
    }

    #[test]
    fn cache_evicts_old_results_without_replacing_pending_work() {
        let mut cache = Cache::default();
        let key = Key::new(Arc::from("x"), false, 14.0, 1.0, gpui::black());
        let view = EntityId::from(1);
        cache.request(&key, view);
        for i in 0..MAX_ENTRIES * 2 {
            let next = Key {
                latex: Arc::from(format!("y_{i}")),
                ..key.clone()
            };
            cache.request(&next, view);
            cache.complete(vec![(next, None)]);
            cache.queue.retain(|queued| queued == &key);
        }
        assert_eq!(cache.entries.len(), MAX_ENTRIES);
        assert!(matches!(cache.entries[&key].state, State::Pending(_)));
        assert_eq!(cache.pending, 1);
    }

    #[test]
    #[ignore = "manual performance measurement"]
    fn benchmark_native_math_cache() {
        let renderer = SvgRenderer::new(Arc::new(()));
        let started = std::time::Instant::now();
        let key = Key::new(
            Arc::from(r"\frac{-b\pm\sqrt{b^2-4ac}}{2a}"),
            true,
            14.0,
            2.0,
            gpui::black(),
        );
        let rendered = Arc::new(render(&key, &renderer).unwrap());
        let cold = started.elapsed();
        let mut cache = Cache::default();
        let view = EntityId::from(1);
        cache.request(&key, view);
        cache.queue.clear();
        cache.complete(vec![(key.clone(), Some(rendered))]);
        let started = std::time::Instant::now();
        for _ in 0..100_000 {
            std::hint::black_box(cache.request(&key, view));
        }
        let warm = started.elapsed();
        assert!(cache.queue.is_empty());
        eprintln!(
            "math: cold typeset+raster {cold:?}; 100000 cached lookups {warm:?} ({:?}/lookup)",
            warm / 100_000
        );
    }
}

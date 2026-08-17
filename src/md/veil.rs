//! Paint-only fade for newly appended streaming Markdown text.
//!
//! The complete text enters layout immediately. Only the colors of newly
//! appended byte ranges animate, so the fade cannot change shaping, wrapping,
//! selection offsets, or row height.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::time::Instant;

use gpui::TextRun;

pub const VEIL_EMA_SEED_MS: f32 = 160.0;
pub const VEIL_MIN_FADE_MS: f32 = 120.0;
pub const VEIL_MAX_FADE_MS: f32 = 400.0;
pub const VEIL_CURVE_POW: f32 = 1.6;
const VEIL_GAP_CLAMP_MS: f32 = 1_000.0;

#[derive(Clone, Debug)]
struct Chunk {
    range: Range<usize>,
    started: Instant,
    duration_ms: f32,
}

pub type VeilSpan = (Range<usize>, f32);

pub fn veil_opacity(progress: f32) -> f32 {
    1.0 - (1.0 - progress.clamp(0.0, 1.0)).powf(VEIL_CURVE_POW)
}

pub fn veil_duration_ms(ema_ms: f32) -> f32 {
    (ema_ms * 3.0).clamp(VEIL_MIN_FADE_MS, VEIL_MAX_FADE_MS)
}

pub fn veil_boost(active_chunks: usize) -> f32 {
    1.0 + 0.3 * active_chunks.saturating_sub(2) as f32
}

pub fn veil_ema_next(ema_ms: f32, gap_ms: f32) -> f32 {
    ema_ms * 0.7 + gap_ms.min(VEIL_GAP_CLAMP_MS) * 0.3
}

#[derive(Debug)]
struct ElementVeil {
    previous: String,
    chunks: Vec<Chunk>,
    ema_ms: f32,
    last_append: Option<Instant>,
}

impl Default for ElementVeil {
    fn default() -> Self {
        Self {
            previous: String::new(),
            chunks: Vec::new(),
            ema_ms: VEIL_EMA_SEED_MS,
            last_append: None,
        }
    }
}

fn common_prefix(a: &str, b: &str) -> usize {
    let mut prefix = a
        .as_bytes()
        .iter()
        .zip(b.as_bytes())
        .take_while(|(left, right)| left == right)
        .count();
    while prefix > 0 && !b.is_char_boundary(prefix) {
        prefix -= 1;
    }
    prefix
}

impl ElementVeil {
    fn seed(&mut self, text: &str) {
        self.previous.clear();
        self.previous.push_str(text);
    }

    fn advance(&mut self, text: &str, now: Instant) -> Vec<VeilSpan> {
        if text != self.previous {
            // A streaming Markdown reparse can replace delimiter characters
            // with styled text. Preserve the common prefix and re-veil only
            // the changed tail instead of flashing the whole block.
            let prefix = common_prefix(&self.previous, text);
            self.chunks.retain_mut(|chunk| {
                chunk.range.end = chunk.range.end.min(prefix);
                chunk.range.start < chunk.range.end
            });
            if text.len() > prefix {
                if let Some(last_append) = self.last_append {
                    let gap_ms = now.saturating_duration_since(last_append).as_secs_f32() * 1_000.0;
                    self.ema_ms = veil_ema_next(self.ema_ms, gap_ms);
                }
                self.last_append = Some(now);
                self.chunks.push(Chunk {
                    range: prefix..text.len(),
                    started: now,
                    duration_ms: veil_duration_ms(self.ema_ms),
                });
            }
            self.previous.clear();
            self.previous.push_str(text);
        }

        let boost = veil_boost(self.chunks.len());
        self.chunks.retain(|chunk| {
            now.saturating_duration_since(chunk.started).as_secs_f32() * 1_000.0 * boost
                < chunk.duration_ms
        });
        let boost = veil_boost(self.chunks.len());
        self.chunks
            .iter()
            .map(|chunk| {
                let elapsed_ms =
                    now.saturating_duration_since(chunk.started).as_secs_f32() * 1_000.0;
                let progress = (elapsed_ms * boost / chunk.duration_ms).clamp(0.0, 1.0);
                (chunk.range.clone(), veil_opacity(progress))
            })
            .collect()
    }

    fn is_fading(&self) -> bool {
        !self.chunks.is_empty()
    }
}

/// Fade state for one Markdown body, keyed by the renderer's stable text
/// element ordinal.
#[derive(Debug, Default)]
pub struct RowVeil {
    elements: HashMap<usize, ElementVeil>,
    seen_this_frame: HashSet<usize>,
    seeding: bool,
}

impl RowVeil {
    /// Existing content is adopted at full opacity on the first render. This
    /// is used when attaching to a response that was already streaming.
    pub fn seeded() -> Self {
        Self {
            elements: HashMap::new(),
            seen_this_frame: HashSet::new(),
            seeding: true,
        }
    }

    pub fn begin_frame(&mut self) {
        self.seen_this_frame.clear();
    }

    pub fn finish_seeding(&mut self) {
        self.seeding = false;
    }

    pub fn finish_frame(&mut self) {
        self.elements
            .retain(|element, _| self.seen_this_frame.contains(element));
        self.finish_seeding();
    }

    pub fn advance(&mut self, element: usize, text: &str, now: Instant) -> Vec<VeilSpan> {
        self.seen_this_frame.insert(element);
        if self.seeding && !self.elements.contains_key(&element) {
            let mut veil = ElementVeil::default();
            veil.seed(text);
            self.elements.insert(element, veil);
            return Vec::new();
        }
        self.elements.entry(element).or_default().advance(text, now)
    }

    pub fn is_fading(&self) -> bool {
        self.elements.values().any(ElementVeil::is_fading)
    }
}

/// Split runs at veil boundaries and multiply only paint colors by the
/// current opacity. The text and total run lengths remain byte-identical.
pub fn apply_veil(runs: Vec<TextRun>, spans: &[VeilSpan]) -> Vec<TextRun> {
    if spans.is_empty() || spans.iter().all(|(_, opacity)| *opacity >= 1.0) {
        return runs;
    }

    let mut output = Vec::with_capacity(runs.len() + spans.len() * 2);
    let mut position = 0;
    for run in runs {
        let start = position;
        let end = start + run.len;
        position = end;
        let mut cuts = vec![start, end];
        for (range, _) in spans {
            if range.start > start && range.start < end {
                cuts.push(range.start);
            }
            if range.end > start && range.end < end {
                cuts.push(range.end);
            }
        }
        cuts.sort_unstable();
        cuts.dedup();

        for interval in cuts.windows(2) {
            let (piece_start, piece_end) = (interval[0], interval[1]);
            let mut piece = run.clone();
            piece.len = piece_end - piece_start;
            if let Some(opacity) = spans
                .iter()
                .find(|(range, _)| range.start <= piece_start && piece_end <= range.end)
                .map(|(_, opacity)| *opacity)
                .filter(|opacity| *opacity < 1.0)
            {
                piece.color = piece.color.opacity(opacity);
                piece.background_color = piece.background_color.map(|color| color.opacity(opacity));
                if let Some(underline) = &mut piece.underline {
                    underline.color = underline.color.map(|color| color.opacity(opacity));
                }
                if let Some(strikethrough) = &mut piece.strikethrough {
                    strikethrough.color = strikethrough.color.map(|color| color.opacity(opacity));
                }
            }
            output.push(piece);
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{TextRun, font};
    use std::time::Duration;

    fn at(base: Instant, millis: u64) -> Instant {
        base + Duration::from_millis(millis)
    }

    fn run(len: usize) -> TextRun {
        TextRun {
            len,
            font: font("Test"),
            color: gpui::white(),
            background_color: None,
            underline: None,
            strikethrough: None,
        }
    }

    #[test]
    fn appended_chunks_fade_once_and_independently() {
        let start = Instant::now();
        let mut veil = ElementVeil::default();
        assert_eq!(veil.advance("one ", start), vec![(0..4, 0.0)]);
        let spans = veil.advance("one two", at(start, 100));
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].0, 0..4);
        assert_eq!(spans[1].0, 4..7);
        assert!(spans[0].1 > spans[1].1);
        assert!(veil.advance("one two", at(start, 600)).is_empty());
        assert!(veil.advance("one two", at(start, 700)).is_empty());
    }

    #[test]
    fn seeded_rows_do_not_refade_existing_content() {
        let start = Instant::now();
        let mut veil = RowVeil::seeded();
        assert!(veil.advance(0, "already here", start).is_empty());
        veil.finish_seeding();
        assert_eq!(
            veil.advance(0, "already here plus", at(start, 100)),
            vec![(12..17, 0.0)]
        );
    }

    #[test]
    fn markdown_rewrites_keep_the_common_prefix() {
        let start = Instant::now();
        let mut veil = ElementVeil::default();
        veil.advance("intro **bol", start);
        let spans = veil.advance("intro bold", at(start, 100));
        assert_eq!(spans[0].0, 0..6);
        assert_eq!(spans[1].0, 6..10);
    }

    #[test]
    fn veil_splits_runs_without_changing_layout_lengths() {
        let runs = vec![run(4), run(6)];
        let faded = apply_veil(runs.clone(), &[(2..8, 0.5)]);
        assert_eq!(
            faded.iter().map(|run| run.len).collect::<Vec<_>>(),
            vec![2, 2, 4, 2]
        );
        assert_eq!(faded.iter().map(|run| run.len).sum::<usize>(), 10);
        assert!(faded.iter().all(|run| run.font == runs[0].font));
        assert_eq!(faded[0].color.a, 1.0);
        assert_eq!(faded[1].color.a, 0.5);
    }

    #[test]
    fn cadence_curve_matches_comet() {
        assert_eq!(veil_duration_ms(160.0), 400.0);
        assert_eq!(veil_duration_ms(30.0), 120.0);
        assert_eq!(veil_boost(2), 1.0);
        assert!((veil_boost(3) - 1.3).abs() < f32::EPSILON);
        assert_eq!(veil_opacity(0.0), 0.0);
        assert_eq!(veil_opacity(1.0), 1.0);
        assert!(veil_opacity(0.5) > 0.5);
    }
}

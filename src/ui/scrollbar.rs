//! Overlay scrollbars for [`ListState`] and [`ScrollHandle`] surfaces.
//!
//! Drawn as a single quad from geometry the list already tracks, with the drag
//! and click listeners registered during paint. That keeps it to one element
//! and no layout children, so the scrollbar costs the transcript essentially
//! nothing per frame.
//!
//! It follows AppKit's overlay scrollers: hidden at rest, revealed while the
//! content moves, held briefly, then faded out — and revealed again, wider,
//! whenever the pointer is over its track.

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{
    App, BorderStyle, Bounds, IntoElement, ListState, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, Pixels, Point, ScrollHandle, Styled, Window, canvas, point, px, quad, size,
};

use crate::theme::Theme;

/// Track width, and the thumb's resting and hovered widths inside it.
const TRACK_WIDTH: f32 = 11.0;
const THUMB_WIDTH: f32 = 5.0;
const THUMB_WIDTH_ACTIVE: f32 = 8.0;
const THUMB_MIN_LENGTH: f32 = 28.0;
const TRACK_INSET: f32 = 2.0;

/// How long the bar stays at full strength after the last scroll, and how long
/// it then takes to fade out. Tuned to feel like AppKit's overlay scrollers.
const HOLD: Duration = Duration::from_millis(900);
const FADE: Duration = Duration::from_millis(350);

/// Cross-frame scrollbar state. The owner holds one per scrollable surface.
#[derive(Debug, Default)]
pub struct ScrollbarState {
    /// While dragging: the pointer's offset inside the thumb, in pixels.
    grab_offset: Cell<Option<f32>>,
    hovered: Cell<bool>,
    /// When the content last moved, which starts the hold-then-fade timer.
    last_scroll: Cell<Option<Instant>>,
    /// Offset at the previous paint, to notice movement.
    last_offset: Cell<Option<Pixels>>,
}

impl ScrollbarState {
    pub fn new() -> Rc<Self> {
        Rc::new(Self::default())
    }

    fn is_grabbed(&self) -> bool {
        self.grab_offset.get().is_some()
    }

    /// Note the current scroll offset, starting the reveal timer when it moved.
    /// The first observation only seeds the baseline, so a transcript that opens
    /// already scrolled to its tail does not flash its scrollbar.
    fn observe(&self, offset: Pixels, now: Instant) {
        match self.last_offset.replace(Some(offset)) {
            Some(previous) if (offset - previous).abs() > px(0.5) => {
                self.last_scroll.set(Some(now));
            }
            _ => {}
        }
    }
}

/// Overlay opacity: solid while hovered or dragging, otherwise held briefly
/// after a scroll and then faded out. Pure, so the timing is testable.
///
/// Under reduce-motion the hold still applies but the ramp does not: the bar
/// goes straight from solid to gone. Skipping the *frames* instead would leave
/// the thumb painted at whatever opacity it last had, because nothing else
/// repaints a resting surface.
fn opacity(
    since_scroll: Option<Duration>,
    hovered: bool,
    grabbed: bool,
    reduce_motion: bool,
) -> f32 {
    if hovered || grabbed {
        return 1.0;
    }
    let Some(elapsed) = since_scroll else {
        return 0.0;
    };
    if elapsed < HOLD {
        return 1.0;
    }
    if reduce_motion {
        return 0.0;
    }
    let fading = (elapsed - HOLD).as_secs_f32() / FADE.as_secs_f32();
    (1.0 - fading).clamp(0.0, 1.0)
}

/// Resolved scrollbar geometry, or `None` when the surface does not scroll.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Geometry {
    /// Thumb rect within the track.
    thumb: Bounds<Pixels>,
    /// Travel available to the thumb along the track.
    travel: Pixels,
    /// Scrollable content beyond the viewport.
    max_offset: Pixels,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Axis {
    Horizontal,
    Vertical,
}

impl Axis {
    fn extent(self, bounds: Bounds<Pixels>) -> Pixels {
        match self {
            Self::Horizontal => bounds.size.width,
            Self::Vertical => bounds.size.height,
        }
    }

    fn coordinate(self, point: Point<Pixels>) -> Pixels {
        match self {
            Self::Horizontal => point.x,
            Self::Vertical => point.y,
        }
    }

    fn start(self, bounds: Bounds<Pixels>) -> Pixels {
        match self {
            Self::Horizontal => bounds.left(),
            Self::Vertical => bounds.top(),
        }
    }
}

/// Compute the thumb rect for a track. Pure, so the mapping between scroll
/// offset and thumb position is unit-testable.
fn geometry(
    track: Bounds<Pixels>,
    axis: Axis,
    viewport_extent: Pixels,
    max_offset: Pixels,
    offset: Pixels,
    thumb_thickness: Pixels,
) -> Option<Geometry> {
    let track_extent = axis.extent(track);
    if viewport_extent <= Pixels::ZERO || max_offset <= px(0.5) || track_extent <= Pixels::ZERO {
        return None;
    }
    let content_extent = viewport_extent + max_offset;
    let thumb_length = (track_extent * (viewport_extent / content_extent))
        .max(px(THUMB_MIN_LENGTH))
        .min(track_extent);
    let travel = (track_extent - thumb_length).max(Pixels::ZERO);
    let progress = (offset / max_offset).clamp(0.0, 1.0);
    let thumb_start = axis.start(track) + travel * progress;
    let thumb = match axis {
        Axis::Horizontal => Bounds::new(
            point(
                thumb_start,
                track.bottom() - thumb_thickness - px(TRACK_INSET),
            ),
            size(thumb_length, thumb_thickness),
        ),
        Axis::Vertical => Bounds::new(
            point(
                track.right() - thumb_thickness - px(TRACK_INSET),
                thumb_start,
            ),
            size(thumb_thickness, thumb_length),
        ),
    };
    Some(Geometry {
        thumb,
        travel,
        max_offset,
    })
}

/// How far through the content a thumb start corresponds to.
fn offset_for_thumb_start(track_start: Pixels, thumb_start: Pixels, geometry: &Geometry) -> Pixels {
    if geometry.travel <= Pixels::ZERO {
        return Pixels::ZERO;
    }
    let progress = ((thumb_start - track_start) / geometry.travel).clamp(0.0, 1.0);
    geometry.max_offset * progress
}

/// The geometry a scrollable surface has to expose. GPUI stores offsets as a
/// non-positive point; implementations report a positive distance so the
/// geometry above reads the obvious way.
pub trait Scrollable {
    fn viewport_extent(&self, axis: Axis) -> Pixels;
    fn max_offset(&self, axis: Axis) -> Pixels;
    fn scrolled(&self, axis: Axis) -> Pixels;
    fn scroll_to(&self, axis: Axis, offset: Pixels);
}

impl Scrollable for ListState {
    fn viewport_extent(&self, axis: Axis) -> Pixels {
        axis.extent(self.viewport_bounds())
    }

    fn max_offset(&self, axis: Axis) -> Pixels {
        axis.coordinate(self.max_offset_for_scrollbar())
    }

    fn scrolled(&self, axis: Axis) -> Pixels {
        -axis.coordinate(self.scroll_px_offset_for_scrollbar())
    }

    fn scroll_to(&self, axis: Axis, offset: Pixels) {
        let current = self.scroll_px_offset_for_scrollbar();
        self.set_offset_from_scrollbar(match axis {
            Axis::Horizontal => point(-offset, current.y),
            Axis::Vertical => point(current.x, -offset),
        });
    }
}

impl Scrollable for ScrollHandle {
    fn viewport_extent(&self, axis: Axis) -> Pixels {
        axis.extent(self.bounds())
    }

    fn max_offset(&self, axis: Axis) -> Pixels {
        axis.coordinate(self.max_offset())
    }

    fn scrolled(&self, axis: Axis) -> Pixels {
        -axis.coordinate(self.offset())
    }

    fn scroll_to(&self, axis: Axis, offset: Pixels) {
        let current = self.offset();
        self.set_offset(match axis {
            Axis::Horizontal => Point::new(-offset, current.y),
            Axis::Vertical => Point::new(current.x, -offset),
        });
    }
}

fn scroll_to(surface: &impl Scrollable, axis: Axis, offset: Pixels, max_offset: Pixels) {
    surface.scroll_to(axis, offset.clamp(Pixels::ZERO, max_offset));
}

fn overlay<S>(surface: &S, state: &Rc<ScrollbarState>, axis: Axis) -> impl IntoElement
where
    S: Scrollable + Clone + 'static,
{
    let surface = surface.clone();
    let state = state.clone();
    let overlay = canvas(
        |_, _, _| (),
        move |track: Bounds<Pixels>, _, window: &mut Window, cx: &mut App| {
            let theme = Theme::current(cx);
            let viewport_extent = surface.viewport_extent(axis);
            let max_offset = Scrollable::max_offset(&surface, axis);
            let offset = surface.scrolled(axis);
            let now = Instant::now();
            state.observe(offset, now);

            let hovered = state.hovered.get();
            let grabbed = state.is_grabbed();
            let active = hovered || grabbed;
            let thumb_thickness = px(if active {
                THUMB_WIDTH_ACTIVE
            } else {
                THUMB_WIDTH
            });

            let Some(geometry) = geometry(
                track,
                axis,
                viewport_extent,
                max_offset,
                offset,
                thumb_thickness,
            ) else {
                // Not scrollable: drop any stale drag so a later resize cannot
                // resume one, and paint nothing.
                state.grab_offset.set(None);
                state.hovered.set(false);
                return;
            };

            let since_scroll = state
                .last_scroll
                .get()
                .map(|last| now.saturating_duration_since(last));
            let opacity = opacity(since_scroll, hovered, grabbed, cx.reduce_motion());
            if opacity > 0.0 {
                window.paint_quad(quad(
                    geometry.thumb,
                    thumb_thickness / 2.0,
                    if active {
                        theme.text_tertiary
                    } else {
                        theme.text_ghost.opacity(0.55)
                    }
                    .opacity(opacity),
                    px(0.0),
                    gpui::transparent_black(),
                    BorderStyle::default(),
                ));
                if !active {
                    // Keep driving frames through the hold and the fade;
                    // nothing else would repaint a resting transcript. The
                    // requests stop on their own once `opacity` reaches zero,
                    // which reduce-motion reaches a whole fade earlier.
                    window.request_animation_frame();
                }
            }

            // Hover is tracked from move events rather than by an interactive
            // child: the bar has to be able to reveal itself while invisible,
            // and a hidden child could not be hovered.
            window.on_mouse_event({
                let state = state.clone();
                move |event: &MouseMoveEvent, phase, window, _| {
                    if phase != gpui::DispatchPhase::Bubble {
                        return;
                    }
                    let hovering = track.contains(&event.position);
                    if state.hovered.replace(hovering) != hovering {
                        window.refresh();
                    }
                }
            });

            window.on_mouse_event({
                let surface = surface.clone();
                let state = state.clone();
                move |event: &MouseDownEvent, phase, window, _| {
                    if phase != gpui::DispatchPhase::Bubble
                        || event.button != MouseButton::Left
                        || !track.contains(&event.position)
                    {
                        return;
                    }
                    let pointer = axis.coordinate(event.position);
                    if geometry.thumb.contains(&event.position) {
                        state
                            .grab_offset
                            .set(Some(f32::from(pointer - axis.start(geometry.thumb))));
                    } else {
                        // A click on bare track centres the thumb there and
                        // begins dragging from its middle.
                        let half = axis.extent(geometry.thumb) / 2.0;
                        state.grab_offset.set(Some(f32::from(half)));
                        scroll_to(
                            &surface,
                            axis,
                            offset_for_thumb_start(axis.start(track), pointer - half, &geometry),
                            geometry.max_offset,
                        );
                    }
                    window.refresh();
                }
            });

            window.on_mouse_event({
                let surface = surface.clone();
                let state = state.clone();
                move |event: &MouseMoveEvent, phase, window, _| {
                    if phase != gpui::DispatchPhase::Bubble {
                        return;
                    }
                    let Some(grab) = state.grab_offset.get() else {
                        return;
                    };
                    scroll_to(
                        &surface,
                        axis,
                        offset_for_thumb_start(
                            axis.start(track),
                            axis.coordinate(event.position) - px(grab),
                            &geometry,
                        ),
                        geometry.max_offset,
                    );
                    window.refresh();
                }
            });

            window.on_mouse_event({
                let state = state.clone();
                move |_: &MouseUpEvent, phase, window, _| {
                    if phase != gpui::DispatchPhase::Bubble || state.grab_offset.get().is_none() {
                        return;
                    }
                    state.grab_offset.set(None);
                    window.refresh();
                }
            });
        },
    )
    .absolute();

    match axis {
        // The horizontal track stops a track-width short of the right edge so
        // it cannot overlap the vertical one. Both register window-level mouse
        // listeners keyed only on `track.contains`, so a shared corner would
        // let one click grab and jump both axes at once. AppKit reserves that
        // corner for neither scroller; so do we.
        Axis::Horizontal => overlay
            .left_0()
            .bottom_0()
            .right(px(TRACK_WIDTH))
            .h(px(TRACK_WIDTH)),
        Axis::Vertical => overlay.top_0().right_0().h_full().w(px(TRACK_WIDTH)),
    }
}

/// An overlay vertical scrollbar pinned to the right edge of its parent.
///
/// The parent must be `relative()`; this element positions itself absolutely
/// and never participates in layout, so adding it cannot change content size.
pub fn vertical<S>(surface: &S, state: &Rc<ScrollbarState>) -> impl IntoElement
where
    S: Scrollable + Clone + 'static,
{
    overlay(surface, state, Axis::Vertical)
}

/// An overlay horizontal scrollbar pinned to the bottom edge of its parent,
/// stopping short of the bottom-right corner so a vertical bar on the same
/// parent keeps sole ownership of it.
///
/// The parent must be `relative()`; this element positions itself absolutely
/// and never participates in layout, so adding it cannot change content size.
pub fn horizontal<S>(surface: &S, state: &Rc<ScrollbarState>) -> impl IntoElement
where
    S: Scrollable + Clone + 'static,
{
    overlay(surface, state, Axis::Horizontal)
}

#[cfg(test)]
mod tests {
    use gpui::{ParentElement, div, prelude::*};

    use super::*;

    fn track() -> Bounds<Pixels> {
        Bounds::new(point(px(500.0), px(100.0)), size(px(11.0), px(400.0)))
    }

    fn horizontal_track() -> Bounds<Pixels> {
        Bounds::new(point(px(100.0), px(500.0)), size(px(400.0), px(11.0)))
    }

    fn vertical_geometry(
        track: Bounds<Pixels>,
        viewport_extent: Pixels,
        max_offset: Pixels,
        offset: Pixels,
        thumb_thickness: Pixels,
    ) -> Option<Geometry> {
        geometry(
            track,
            Axis::Vertical,
            viewport_extent,
            max_offset,
            offset,
            thumb_thickness,
        )
    }

    /// `opacity` with motion allowed, which is every case but the one test
    /// below that is about reduce-motion.
    fn opacity(since_scroll: Option<Duration>, hovered: bool, grabbed: bool) -> f32 {
        super::opacity(since_scroll, hovered, grabbed, false)
    }

    #[test]
    fn the_bar_rests_hidden_and_reveals_on_scroll() {
        // Nothing has scrolled yet, so there is nothing to show.
        assert_eq!(opacity(None, false, false), 0.0);

        // A scroll reveals it at full strength, and it holds there.
        assert_eq!(opacity(Some(Duration::ZERO), false, false), 1.0);
        assert_eq!(
            opacity(Some(HOLD - Duration::from_millis(1)), false, false),
            1.0
        );

        // Then it fades out over FADE and stays gone.
        let midway = opacity(Some(HOLD + FADE / 2), false, false);
        assert!(
            (0.4..0.6).contains(&midway),
            "expected a half fade, got {midway}"
        );
        assert_eq!(opacity(Some(HOLD + FADE), false, false), 0.0);
        assert_eq!(opacity(Some(HOLD + FADE * 10), false, false), 0.0);
    }

    #[test]
    fn hovering_or_dragging_pins_the_bar_visible() {
        // Long past the fade, but the pointer is on the track.
        assert_eq!(opacity(Some(HOLD + FADE * 10), true, false), 1.0);
        // Mid-drag the pointer may leave the track entirely.
        assert_eq!(opacity(Some(HOLD + FADE * 10), false, true), 1.0);
        // A drag that began before anything scrolled still shows.
        assert_eq!(opacity(None, false, true), 1.0);
    }

    /// Reduce-motion drops the ramp, not the disappearance: the thumb must not
    /// be left painted at a resting opacity forever.
    #[test]
    fn reduce_motion_hides_the_bar_without_a_fade() {
        let reduced = |since| super::opacity(Some(since), false, false, true);

        // The hold is not motion, so it stays.
        assert_eq!(reduced(Duration::ZERO), 1.0);
        assert_eq!(reduced(HOLD - Duration::from_millis(1)), 1.0);

        // Where the fade would have been, there is nothing at all.
        assert_eq!(reduced(HOLD), 0.0);
        assert_eq!(reduced(HOLD + FADE / 2), 0.0);
        assert_eq!(reduced(HOLD + FADE * 10), 0.0);

        // Hover and drag still pin it, reduce-motion or not.
        assert_eq!(super::opacity(None, true, false, true), 1.0);
        assert_eq!(super::opacity(Some(HOLD + FADE), false, true, true), 1.0);
    }

    #[test]
    fn the_first_observed_offset_only_seeds_the_baseline() {
        let state = ScrollbarState::default();
        let start = Instant::now();

        // Opening a transcript already scrolled to its tail must not flash.
        state.observe(px(4_000.0), start);
        assert_eq!(state.last_scroll.get(), None);

        // A real movement starts the timer.
        state.observe(px(3_900.0), start);
        assert!(state.last_scroll.get().is_some());

        // Sub-pixel jitter from remeasurement does not count as movement.
        state.last_scroll.set(None);
        state.observe(px(3_900.2), start);
        assert_eq!(state.last_scroll.get(), None);
    }

    #[test]
    fn a_surface_that_does_not_scroll_has_no_thumb() {
        assert!(
            vertical_geometry(track(), px(400.0), Pixels::ZERO, Pixels::ZERO, px(5.0)).is_none()
        );
        assert!(
            vertical_geometry(track(), Pixels::ZERO, px(900.0), Pixels::ZERO, px(5.0)).is_none()
        );
    }

    #[test]
    fn thumb_height_tracks_the_visible_fraction() {
        // Viewport is a quarter of the content, so the thumb is a quarter of
        // the track.
        let geometry =
            vertical_geometry(track(), px(400.0), px(1200.0), Pixels::ZERO, px(5.0)).unwrap();
        assert_eq!(geometry.thumb.size.height, px(100.0));
        assert_eq!(geometry.thumb.top(), px(100.0));
        assert_eq!(geometry.travel, px(300.0));
    }

    #[test]
    fn a_tiny_visible_fraction_still_leaves_a_grabbable_thumb() {
        let geometry =
            vertical_geometry(track(), px(400.0), px(100_000.0), Pixels::ZERO, px(5.0)).unwrap();
        assert_eq!(geometry.thumb.size.height, px(THUMB_MIN_LENGTH));
    }

    #[test]
    fn thumb_position_and_offset_are_inverse() {
        let track = track();
        let geometry = vertical_geometry(track, px(400.0), px(1200.0), px(600.0), px(5.0)).unwrap();
        // Halfway down the content puts the thumb halfway along its travel.
        assert_eq!(geometry.thumb.top(), track.top() + px(150.0));
        assert_eq!(
            offset_for_thumb_start(track.top(), geometry.thumb.top(), &geometry),
            px(600.0)
        );
    }

    #[test]
    fn horizontal_thumb_tracks_width_and_offset() {
        let track = horizontal_track();
        let geometry = geometry(
            track,
            Axis::Horizontal,
            px(400.0),
            px(1200.0),
            px(600.0),
            px(5.0),
        )
        .unwrap();
        assert_eq!(geometry.thumb.size.width, px(100.0));
        assert_eq!(geometry.thumb.left(), track.left() + px(150.0));
        assert_eq!(
            offset_for_thumb_start(track.left(), geometry.thumb.left(), &geometry),
            px(600.0)
        );
    }

    #[test]
    fn offsets_clamp_at_both_ends() {
        let track = track();
        let geometry =
            vertical_geometry(track, px(400.0), px(1200.0), px(1200.0), px(5.0)).unwrap();
        assert_eq!(
            geometry.thumb.top(),
            track.bottom() - geometry.thumb.size.height
        );

        assert_eq!(
            offset_for_thumb_start(track.top(), track.top() - px(9_999.0), &geometry),
            Pixels::ZERO
        );
        assert_eq!(
            offset_for_thumb_start(track.top(), track.bottom() + px(9_999.0), &geometry),
            px(1200.0)
        );
    }

    #[test]
    fn overscrolled_offsets_do_not_push_the_thumb_past_the_track() {
        let track = track();
        // A momentum overscroll can report more than max for a frame.
        let geometry =
            vertical_geometry(track, px(400.0), px(1200.0), px(5000.0), px(5.0)).unwrap();
        assert!(geometry.thumb.bottom() <= track.bottom() + px(0.001));
    }

    /// Both bars register window-level listeners gated only on their own track
    /// bounds, so an overlapping corner would let one press grab both axes and
    /// the following drag move the content diagonally.
    #[gpui::test]
    fn the_shared_corner_belongs_to_one_bar_only(cx: &mut gpui::TestAppContext) {
        const PANE: f32 = 200.0;

        struct BothBars {
            scroll: ScrollHandle,
            vertical: Rc<ScrollbarState>,
            horizontal: Rc<ScrollbarState>,
        }

        impl gpui::Render for BothBars {
            fn render(&mut self, _: &mut Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
                div()
                    .w(px(PANE))
                    .h(px(PANE))
                    .relative()
                    .child(
                        div()
                            .id("corner-test-scroll")
                            .size_full()
                            .flex()
                            .overflow_scroll()
                            .track_scroll(&self.scroll)
                            .child(div().w(px(1_000.0)).h(px(1_000.0)).flex_none()),
                    )
                    .child(vertical(&self.scroll, &self.vertical))
                    .child(horizontal(&self.scroll, &self.horizontal))
            }
        }

        let scroll = ScrollHandle::new();
        let view_scroll = scroll.clone();
        let (_, cx) = cx.add_window_view(move |_, _| BothBars {
            scroll: view_scroll,
            vertical: ScrollbarState::new(),
            horizontal: ScrollbarState::new(),
        });
        assert!(
            scroll.max_offset().x > px(0.0) && scroll.max_offset().y > px(0.0),
            "the harness must scroll on both axes"
        );

        // Press in the bottom-right corner, inside both tracks were they to
        // overlap. Only the vertical bar may claim it.
        cx.simulate_event(MouseDownEvent {
            button: MouseButton::Left,
            position: point(px(PANE - 3.0), px(PANE - 3.0)),
            modifiers: Default::default(),
            click_count: 1,
            first_mouse: false,
        });
        assert_eq!(
            scroll.offset().x,
            px(0.0),
            "a press in the corner must not jump the horizontal axis"
        );
        assert!(
            scroll.offset().y < px(0.0),
            "the vertical bar still owns the corner"
        );
    }
}

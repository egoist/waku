//! Overlay scrollbar for a virtualized [`ListState`].
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
const THUMB_MIN_HEIGHT: f32 = 28.0;
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
    /// A hold-expiry wake is in flight; see `arm_fade_wake`.
    fade_wake_armed: Cell<bool>,
}

impl ScrollbarState {
    pub fn new() -> Rc<Self> {
        Rc::new(Self::default())
    }

    /// True while the thumb is held. The bar writes offsets straight into the
    /// surface without going through its scroll handler, so a surface that
    /// tracks its own scroll intent has no other way to hear about a drag.
    pub fn is_grabbed(&self) -> bool {
        self.grab_offset.get().is_some()
    }

    /// True while the pointer is over the track or a drag is in progress.
    /// Bubble-phase mouse listeners run in reverse registration order, so a
    /// surface painted beneath the bar hears these events too and must be able
    /// to ignore the ones the bar is handling.
    pub fn engaged(&self) -> bool {
        self.hovered.get() || self.is_grabbed()
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
fn opacity(since_scroll: Option<Duration>, hovered: bool, grabbed: bool) -> f32 {
    if hovered || grabbed {
        return 1.0;
    }
    let Some(elapsed) = since_scroll else {
        return 0.0;
    };
    if elapsed < HOLD {
        return 1.0;
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

/// Compute the thumb rect for a track. Pure, so the mapping between scroll
/// offset and thumb position is unit-testable.
fn geometry(
    track: Bounds<Pixels>,
    viewport_height: Pixels,
    max_offset: Pixels,
    offset: Pixels,
    thumb_width: Pixels,
) -> Option<Geometry> {
    if viewport_height <= Pixels::ZERO || max_offset <= px(0.5) || track.size.height <= Pixels::ZERO
    {
        return None;
    }
    let content_height = viewport_height + max_offset;
    let track_height = track.size.height;
    let thumb_height = (track_height * (viewport_height / content_height))
        .max(px(THUMB_MIN_HEIGHT))
        .min(track_height);
    let travel = (track_height - thumb_height).max(Pixels::ZERO);
    let progress = (offset / max_offset).clamp(0.0, 1.0);
    Some(Geometry {
        thumb: Bounds::new(
            point(
                track.right() - thumb_width - px(TRACK_INSET),
                track.top() + travel * progress,
            ),
            size(thumb_width, thumb_height),
        ),
        travel,
        max_offset,
    })
}

/// How far down the content a thumb top of `thumb_top` corresponds to.
fn offset_for_thumb_top(track_top: Pixels, thumb_top: Pixels, geometry: &Geometry) -> Pixels {
    if geometry.travel <= Pixels::ZERO {
        return Pixels::ZERO;
    }
    let progress = ((thumb_top - track_top) / geometry.travel).clamp(0.0, 1.0);
    geometry.max_offset * progress
}

/// The two things a scrollable surface has to expose. GPUI stores both kinds of
/// offset as a non-positive y; implementations report a downward distance so the
/// geometry above reads the obvious way.
pub trait Scrollable {
    /// Height of the visible area.
    fn viewport_height(&self) -> Pixels;
    /// Content height beyond the viewport.
    fn max_offset(&self) -> Pixels;
    /// How far the content is currently scrolled down.
    fn scrolled(&self) -> Pixels;
    fn scroll_to(&self, offset: Pixels);
}

impl Scrollable for ListState {
    fn viewport_height(&self) -> Pixels {
        self.viewport_bounds().size.height
    }

    fn max_offset(&self) -> Pixels {
        self.max_offset_for_scrollbar().y
    }

    fn scrolled(&self) -> Pixels {
        -self.scroll_px_offset_for_scrollbar().y
    }

    fn scroll_to(&self, offset: Pixels) {
        self.set_offset_from_scrollbar(point(Pixels::ZERO, -offset));
    }
}

impl Scrollable for ScrollHandle {
    fn viewport_height(&self) -> Pixels {
        self.bounds().size.height
    }

    fn max_offset(&self) -> Pixels {
        self.max_offset().y
    }

    fn scrolled(&self) -> Pixels {
        -self.offset().y
    }

    fn scroll_to(&self, offset: Pixels) {
        let x = self.offset().x;
        self.set_offset(Point::new(x, -offset));
    }
}

fn scroll_to(surface: &impl Scrollable, offset: Pixels, max_offset: Pixels) {
    surface.scroll_to(offset.clamp(Pixels::ZERO, max_offset));
}

/// One in-flight wake for the end of the reveal hold. If the content keeps
/// moving, the paint that this wake triggers finds the hold extended and arms
/// the next wake — one timer alive at a time, one no-op frame per expiry.
fn arm_fade_wake(state: &Rc<ScrollbarState>, view: gpui::EntityId, delay: Duration, cx: &mut App) {
    if state.fade_wake_armed.replace(true) {
        return;
    }
    let state = state.clone();
    cx.spawn(async move |cx| {
        cx.background_executor()
            .timer(delay + Duration::from_millis(16))
            .await;
        state.fade_wake_armed.set(false);
        cx.update(|cx| cx.notify(view));
    })
    .detach();
}

/// An overlay vertical scrollbar pinned to the right edge of its parent.
///
/// The parent must be `relative()`; this element positions itself absolutely
/// and never participates in layout, so adding it cannot change content size.
pub fn vertical<S>(surface: &S, state: &Rc<ScrollbarState>) -> impl IntoElement + use<S>
where
    S: Scrollable + Clone + 'static,
{
    let list = surface.clone();
    let state = state.clone();
    canvas(
        |_, _, _| (),
        move |track: Bounds<Pixels>, _, window: &mut Window, cx: &mut App| {
            let theme = Theme::current(cx);
            let viewport_height = list.viewport_height();
            let max_offset = Scrollable::max_offset(&list);
            let offset = list.scrolled();
            let now = Instant::now();
            state.observe(offset, now);

            let hovered = state.hovered.get();
            let grabbed = state.is_grabbed();
            let active = hovered || grabbed;
            let thumb_width = px(if active {
                THUMB_WIDTH_ACTIVE
            } else {
                THUMB_WIDTH
            });

            let Some(geometry) = geometry(track, viewport_height, max_offset, offset, thumb_width)
            else {
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
            let opacity = opacity(since_scroll, hovered, grabbed);
            if opacity > 0.0 {
                window.paint_quad(quad(
                    geometry.thumb,
                    thumb_width / 2.0,
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
                    match since_scroll {
                        // The hold is constant-opacity: it needs no repaints,
                        // only a wake at the moment the fade should begin. A
                        // streaming transcript moves its content every commit
                        // and so holds its bar for the whole turn — driving
                        // frames through that hold pinned the pane at pulse
                        // rate for nothing.
                        Some(elapsed) if elapsed < HOLD => {
                            arm_fade_wake(&state, window.current_view(), HOLD - elapsed, cx);
                        }
                        // The fade itself animates; ride the shared pulse
                        // clock, which parks shortly after the bar hides.
                        _ => super::motion::pulse_lease(window.current_view(), cx),
                    }
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
                let list = list.clone();
                let state = state.clone();
                move |event: &MouseDownEvent, phase, window, _| {
                    if phase != gpui::DispatchPhase::Bubble
                        || event.button != MouseButton::Left
                        || !track.contains(&event.position)
                    {
                        return;
                    }
                    if geometry.thumb.contains(&event.position) {
                        state
                            .grab_offset
                            .set(Some(f32::from(event.position.y - geometry.thumb.top())));
                    } else {
                        // A click on bare track centres the thumb there and
                        // begins dragging from its middle.
                        let half = geometry.thumb.size.height / 2.0;
                        state.grab_offset.set(Some(f32::from(half)));
                        scroll_to(
                            &list,
                            offset_for_thumb_top(track.top(), event.position.y - half, &geometry),
                            geometry.max_offset,
                        );
                    }
                    window.refresh();
                }
            });

            window.on_mouse_event({
                let list = list.clone();
                let state = state.clone();
                move |event: &MouseMoveEvent, phase, window, _| {
                    if phase != gpui::DispatchPhase::Bubble {
                        return;
                    }
                    let Some(grab) = state.grab_offset.get() else {
                        return;
                    };
                    scroll_to(
                        &list,
                        offset_for_thumb_top(track.top(), event.position.y - px(grab), &geometry),
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
    .absolute()
    .top_0()
    .right_0()
    .h_full()
    .w(px(TRACK_WIDTH))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track() -> Bounds<Pixels> {
        Bounds::new(point(px(500.0), px(100.0)), size(px(11.0), px(400.0)))
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
        assert!(geometry(track(), px(400.0), Pixels::ZERO, Pixels::ZERO, px(5.0)).is_none());
        assert!(geometry(track(), Pixels::ZERO, px(900.0), Pixels::ZERO, px(5.0)).is_none());
    }

    #[test]
    fn thumb_height_tracks_the_visible_fraction() {
        // Viewport is a quarter of the content, so the thumb is a quarter of
        // the track.
        let geometry = geometry(track(), px(400.0), px(1200.0), Pixels::ZERO, px(5.0)).unwrap();
        assert_eq!(geometry.thumb.size.height, px(100.0));
        assert_eq!(geometry.thumb.top(), px(100.0));
        assert_eq!(geometry.travel, px(300.0));
    }

    #[test]
    fn a_tiny_visible_fraction_still_leaves_a_grabbable_thumb() {
        let geometry = geometry(track(), px(400.0), px(100_000.0), Pixels::ZERO, px(5.0)).unwrap();
        assert_eq!(geometry.thumb.size.height, px(THUMB_MIN_HEIGHT));
    }

    #[test]
    fn thumb_position_and_offset_are_inverse() {
        let track = track();
        let geometry = geometry(track, px(400.0), px(1200.0), px(600.0), px(5.0)).unwrap();
        // Halfway down the content puts the thumb halfway along its travel.
        assert_eq!(geometry.thumb.top(), track.top() + px(150.0));
        assert_eq!(
            offset_for_thumb_top(track.top(), geometry.thumb.top(), &geometry),
            px(600.0)
        );
    }

    #[test]
    fn offsets_clamp_at_both_ends() {
        let track = track();
        let geometry = geometry(track, px(400.0), px(1200.0), px(1200.0), px(5.0)).unwrap();
        assert_eq!(
            geometry.thumb.top(),
            track.bottom() - geometry.thumb.size.height
        );

        assert_eq!(
            offset_for_thumb_top(track.top(), track.top() - px(9_999.0), &geometry),
            Pixels::ZERO
        );
        assert_eq!(
            offset_for_thumb_top(track.top(), track.bottom() + px(9_999.0), &geometry),
            px(1200.0)
        );
    }

    #[test]
    fn overscrolled_offsets_do_not_push_the_thumb_past_the_track() {
        let track = track();
        // A momentum overscroll can report more than max for a frame.
        let geometry = geometry(track, px(400.0), px(1200.0), px(5000.0), px(5.0)).unwrap();
        assert!(geometry.thumb.bottom() <= track.bottom() + px(0.001));
    }
}

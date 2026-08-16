//! Shared pulse clock for the repeating loaders.
//!
//! Ported from Zeron's motion kit (<https://github.com/zeronsh/comet>, MIT).
//! A repeating `with_animation` element requests a redraw every display frame
//! for as long as it is mounted — one working row pinned the whole window at
//! 120 Hz on a ProMotion panel. Loaders instead read their phase from one
//! shared clock: it ticks at ~30 fps, notifies only views that painted a
//! loader recently, and parks itself once the last lease lapses, so a window
//! with no loader mounted schedules nothing at all. Every loader shares one
//! epoch, keeping multi-instance loaders phase-locked.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, App, EntityId, Global, IntoElement, RenderOnce, Svg, Transformation, Window,
    percentage,
};

/// Repeat-tick interval (~30 fps): visually equivalent for these chunky
/// pulses and spins at a quarter of a ProMotion display's redraws.
const PULSE_TICK: Duration = Duration::from_millis(33);

/// How long a view stays on the tick list after it last painted a loader. One
/// lease outlives a few missed frames; an unmounted loader stops renewing and
/// its view drops off, letting the clock park.
const PULSE_LEASE: Duration = Duration::from_millis(300);

/// The rotating `loader-circle` spinners' period.
const SPINNER_PERIOD: Duration = Duration::from_millis(900);

struct Lease {
    until: Instant,
    /// Notify this view every `stride`-th tick. A view's whole subtree
    /// rebuilds per notify, so a loader on an expensive surface can trade
    /// animation granularity for a cheaper cadence.
    stride: u32,
}

struct PulseClock {
    epoch: Instant,
    leases: HashMap<EntityId, Lease>,
    ticks: u64,
    running: bool,
}

impl Global for PulseClock {}

impl Default for PulseClock {
    fn default() -> Self {
        Self {
            epoch: Instant::now(),
            leases: HashMap::new(),
            ticks: 0,
            running: false,
        }
    }
}

/// Keep `view` re-rendering at [`PULSE_TICK`] until the lease lapses. A caller
/// that stops leasing stops being notified, and the clock parks once no
/// leases remain — quiescence needs no unsubscribe step.
pub fn pulse_lease(view: EntityId, cx: &mut App) {
    pulse_lease_with_stride(view, 1, cx);
}

/// [`pulse_lease`] at every second tick (~15 fps), for animations whose view
/// is expensive to rebuild and whose motion survives the coarser step — a
/// notify re-renders the view's whole subtree, so cadence is priced per
/// tick, not per animation.
pub fn pulse_lease_slow(view: EntityId, cx: &mut App) {
    pulse_lease_with_stride(view, 2, cx);
}

fn pulse_lease_with_stride(view: EntityId, stride: u32, cx: &mut App) {
    let clock = cx.default_global::<PulseClock>();
    let until = Instant::now() + PULSE_LEASE;
    // A view hosting both a full-rate and a strided loader keeps full rate.
    clock
        .leases
        .entry(view)
        .and_modify(|lease| {
            lease.until = until;
            lease.stride = lease.stride.min(stride);
        })
        .or_insert(Lease { until, stride });
    if clock.running {
        return;
    }
    clock.running = true;
    cx.spawn(async move |cx| {
        loop {
            cx.background_executor().timer(PULSE_TICK).await;
            let parked = cx.update(|cx| {
                let clock = cx.default_global::<PulseClock>();
                let now = Instant::now();
                clock.ticks += 1;
                let ticks = clock.ticks;
                clock.leases.retain(|_, lease| lease.until > now);
                if clock.leases.is_empty() {
                    clock.running = false;
                    return true;
                }
                let due = clock
                    .leases
                    .iter_mut()
                    .filter(|(_, lease)| ticks % lease.stride.max(1) as u64 == 0)
                    .map(|(view, lease)| {
                        // Strides re-establish on the render this notify
                        // triggers; without the reset, one full-rate lease
                        // would drag its view's cadence down permanently.
                        lease.stride = u32::MAX;
                        *view
                    })
                    .collect::<Vec<_>>();
                for view in due {
                    cx.notify(view);
                }
                false
            });
            if parked {
                break;
            }
        }
    })
    .detach();
}

/// Phase `[0,1)` of a repeating cycle of `period`, plus a lease keeping `view`
/// re-rendering while its loader stays mounted. Under reduce-motion this is a
/// constant 0 — the cycle's first frame, matching what a repeating
/// `with_animation` held — and nothing is scheduled.
fn pulse_phase(period: Duration, stride: u32, view: EntityId, cx: &mut App) -> f32 {
    if cx.reduce_motion() {
        return 0.0;
    }
    let clock = cx.default_global::<PulseClock>();
    let phase = (clock.epoch.elapsed().as_secs_f32() / period.as_secs_f32()).fract();
    pulse_lease_with_stride(view, stride, cx);
    phase
}

/// A loader element styled from the shared clock's phase. Resolving the phase
/// is deferred to render, where the owning view is known, so call sites need
/// neither a `Window` nor an `EntityId` in scope.
pub fn pulse(period: Duration, render: impl FnOnce(f32) -> AnyElement + 'static) -> Pulse {
    Pulse {
        period,
        stride: 1,
        render: Box::new(render),
    }
}

/// A rotating loader icon riding the shared clock.
pub fn spin(icon: Svg) -> AnyElement {
    spin_with_stride(icon, 1)
}

/// A rotating loader at every second tick (~15 fps — the classic
/// discrete-step spinner cadence). For loaders on expensive surfaces: the
/// sidebar rebuilds its whole subtree per notify, and a session row's working
/// spinner is not worth pricing that at full rate.
pub fn spin_slow(icon: Svg) -> AnyElement {
    spin_with_stride(icon, 2)
}

fn spin_with_stride(icon: Svg, stride: u32) -> AnyElement {
    let mut pulse = pulse(SPINNER_PERIOD, move |phase| {
        icon.with_transformation(Transformation::rotate(percentage(phase)))
            .into_any_element()
    });
    pulse.stride = stride;
    pulse.into_any_element()
}

#[derive(IntoElement)]
pub struct Pulse {
    period: Duration,
    stride: u32,
    render: Box<dyn FnOnce(f32) -> AnyElement>,
}

impl Pulse {
    /// Tick every `stride`-th pulse instead of every one. A view's whole
    /// subtree rebuilds per notify — the pane ticks at the fastest of its
    /// lessees — so a loader mounted for a whole turn on an expensive
    /// surface should ride the coarser cadence.
    pub fn every(mut self, stride: u32) -> Self {
        self.stride = stride.max(1);
        self
    }
}

impl RenderOnce for Pulse {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let phase = pulse_phase(self.period, self.stride, window.current_view(), cx);
        (self.render)(phase)
    }
}

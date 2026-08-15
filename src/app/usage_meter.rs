//! The usage meter under the composer: a circular context-window gauge that
//! opens a panel with the session's context occupancy and the account's
//! rate-limit lanes, mirroring Claude Code's `/usage` rows. Context numbers
//! stream in from every provider transport that reports them; plan lanes come
//! from the Claude OAuth endpoint (fetched off-thread in [`crate::usage`]),
//! OpenCode Go's usage endpoint, and Codex's own rate-limit
//! notifications. Frames read only snapshots stored on the entity.

use gpui::{PathBuilder, relative};

use super::*;
use crate::usage::{PlanUsage, format_tokens, reset_label};

const USAGE_METER_MENU_ID: &str = "usage-meter";

/// Providers with an account-level plan fetcher. Codex additionally refreshes
/// live from its own stream notifications.
pub(super) const PLAN_USAGE_PROVIDERS: [ProviderKind; 4] = [
    ProviderKind::Claude,
    ProviderKind::Codex,
    ProviderKind::OpenCode,
    ProviderKind::Grok,
];

/// Refresh cadences for the plan snapshots. Quota moves only when turns run,
/// so idle refreshes stay rare; a settled turn or a just-opened panel asks
/// sooner. Grok's fetch spawns a probe process, so its idle cadence is wider.
const PLAN_USAGE_REFRESH: Duration = Duration::from_secs(300);
const PLAN_USAGE_REFRESH_GROK: Duration = Duration::from_secs(600);
const PLAN_USAGE_REFRESH_STALE: Duration = Duration::from_secs(30);
const PLAN_USAGE_RETRY: Duration = Duration::from_secs(90);

impl Waku {
    /// Start background fetches of any plan meters whose snapshot is due.
    /// The slow maintenance clock and explicit panel-open requests call this;
    /// guards keep it to one in-flight fetch per provider.
    pub(super) fn maybe_refresh_plan_usage(&mut self, cx: &mut Context<Self>) {
        // Disabling a provider only stops it backing new sessions; a session
        // already locked to it keeps running, and while one is selected its
        // usage panel still owes the account meters. Without this the panel
        // would show its loading skeleton forever: fetchable provider, no
        // snapshot, and no fetch ever allowed to start.
        let selected_provider = self.selected_session().map(|session| session.provider);
        for provider in PLAN_USAGE_PROVIDERS {
            if self.plan_usage_pending.contains(&provider)
                || (!self.provider_enabled(provider) && selected_provider != Some(provider))
            {
                continue;
            }
            let interval = if self.plan_usage_error.contains_key(&provider) {
                PLAN_USAGE_RETRY
            } else if self.plan_usage_stale.contains(&provider) {
                PLAN_USAGE_REFRESH_STALE
            } else if provider == ProviderKind::Grok {
                PLAN_USAGE_REFRESH_GROK
            } else {
                PLAN_USAGE_REFRESH
            };
            if self
                .plan_usage_checked_at
                .get(&provider)
                .is_some_and(|checked| checked.elapsed() < interval)
            {
                continue;
            }
            self.plan_usage_pending.insert(provider);
            let tx = self.plan_usage_tx.clone();
            let event_wake = self.event_wake_tx.clone();
            let claude_version = self
                .provider_versions
                .get(&ProviderKind::Claude)
                .cloned()
                .flatten();
            let binary_override = self.state.provider_binary_overrides.get(&provider).cloned();
            let daemon = self.daemon.client();
            cx.background_executor()
                .spawn(async move {
                    let result = match daemon.request(
                        Uuid::nil(),
                        Uuid::nil(),
                        waku_client::Command::FetchPlanUsage {
                            provider,
                            binary_override,
                            cli_version: claude_version,
                        },
                    ) {
                        Ok(waku_client::ResponsePayload::PlanUsage { usage }) => Ok(usage),
                        Ok(_) => Err(anyhow::anyhow!(
                            "the daemon returned an invalid plan usage response"
                        )),
                        Err(error) => Err(error),
                    };
                    if tx
                        .send((provider, result.map_err(|error| format!("{error:#}"))))
                        .is_ok()
                    {
                        signal_event_pump(&event_wake);
                    }
                })
                .detach();
        }
    }

    pub(super) fn drain_plan_usage_events(&mut self) -> bool {
        let mut changed = false;
        while let Ok((provider, result)) = self.plan_usage_events.try_recv() {
            self.plan_usage_pending.remove(&provider);
            self.plan_usage_stale.remove(&provider);
            self.plan_usage_checked_at.insert(provider, Instant::now());
            match result {
                Ok(Some(usage)) => {
                    changed |= self.plan_usage.get(&provider) != Some(&usage)
                        || self.plan_usage_error.contains_key(&provider)
                        || self.plan_usage_unconfigured.contains(&provider);
                    self.plan_usage.insert(provider, usage);
                    self.plan_usage_error.remove(&provider);
                    self.plan_usage_unconfigured.remove(&provider);
                }
                Ok(None) => {
                    let had_usage = self.plan_usage.remove(&provider).is_some();
                    let had_error = self.plan_usage_error.remove(&provider).is_some();
                    let newly_unconfigured = self.plan_usage_unconfigured.insert(provider);
                    changed |= had_usage || had_error || newly_unconfigured;
                }
                Err(error) => {
                    let was_unconfigured = self.plan_usage_unconfigured.remove(&provider);
                    changed |=
                        self.plan_usage_error.get(&provider) != Some(&error) || was_unconfigured;
                    // Keep any previous snapshot; stale meters with reset
                    // times still self-correct visually.
                    self.plan_usage_error.insert(provider, error);
                }
            }
        }
        changed
    }

    /// Whether the footer shows the gauge. Always true with a session
    /// selected — an empty ring is the honest "nothing measured yet" state,
    /// and hiding it would make the control feel intermittent.
    pub(super) fn usage_meter_available(&self) -> bool {
        self.selected_session().is_some()
    }

    /// Primary modifier + U: toggle the usage panel as if its footer trigger were clicked.
    pub(super) fn toggle_usage_panel_action(
        &mut self,
        _: &ToggleUsagePanel,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.active_page.is_some() || !self.usage_meter_available() {
            return;
        }
        let menus = self.menus.borrow();
        let Some(handle) = menus.get(USAGE_METER_MENU_ID).cloned() else {
            return;
        };
        // A keyboard toggle produces no mouse-down for another open menu's
        // dismiss-on-down-out to see, so close the rest here.
        let other_open: Vec<_> = menus
            .iter()
            .filter(|(id, other)| id.as_ref() != USAGE_METER_MENU_ID && other.is_open())
            .map(|(_, other)| other.clone())
            .collect();
        drop(menus);
        window.defer(cx, move |window, cx| {
            for menu in other_open {
                menu.close(window, cx);
            }
            crate::ui::menu::toggle_popover(&handle, MenuAlign::AboveRight, window, cx);
        });
    }

    /// The footer's circular context gauge plus its anchored panel. `None`
    /// while there is nothing to show for the selected session's provider.
    pub(super) fn render_usage_meter(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.usage_meter_available() {
            return None;
        }
        let session = self.selected_session()?;
        let provider = session.provider;
        let context = session.context_usage;
        let theme = Theme::current(cx);
        let plan = self.plan_usage.get(&provider).cloned();
        let error = self.plan_usage_error.get(&provider).cloned();
        // Fetchable but nothing cached yet: the panel shows a skeleton
        // whether the fetch is already in flight or lands on the next tick.
        let plan_loading = plan.is_none()
            && error.is_none()
            && !self.plan_usage_unconfigured.contains(&provider)
            && PLAN_USAGE_PROVIDERS.contains(&provider);

        let weak = cx.entity().downgrade();
        let handle = self.menu_handle_with(USAGE_METER_MENU_ID, cx, move |open, window, cx| {
            if open {
                let mut card_focus = None;
                let _ = weak.update(cx, |this, cx| {
                    // An opening panel wants fresh numbers; the pump honors
                    // the stale flag on its next tick once the backoff allows.
                    if let Some(provider) = this
                        .selected_session()
                        .map(|session| session.provider)
                        .filter(|provider| PLAN_USAGE_PROVIDERS.contains(provider))
                    {
                        this.plan_usage_stale.insert(provider);
                    }
                    this.maybe_refresh_plan_usage(cx);
                    card_focus = this
                        .menus
                        .borrow()
                        .get(USAGE_METER_MENU_ID)
                        .map(|handle| handle.focus_handle().clone());
                    cx.notify();
                });
                // The card is deferred, so its focus handle joins the
                // dispatch tree only after the deferred draw — the same
                // two-frame wait the menus use. Focused, the card's menu
                // context is what lets `escape` dismiss it.
                if let Some(focus) = card_focus {
                    window.on_next_frame(move |window, _| {
                        window.on_next_frame(move |window, cx| window.focus(&focus, cx));
                    });
                }
            } else {
                let mut composer_focus = None;
                let _ = weak.update(cx, |this, cx| {
                    composer_focus = Some(this.composer.read(cx).focus());
                    cx.notify();
                });
                if let Some(focus) = composer_focus {
                    window.focus(&focus, cx);
                }
            }
        });

        let percent = context.and_then(context_percent);
        let fill = match percent {
            Some(percent) if percent >= 95.0 => theme.danger,
            Some(percent) if percent >= 80.0 => theme.warning,
            _ => theme.gauge,
        };
        let tooltip = match (&error, percent) {
            (Some(error), _) => SharedString::from(tr!("usage.refresh_failed", error = error)),
            (None, Some(percent)) => SharedString::from(tr!(
                "usage.context_used",
                percent = format!("{percent:.0}"),
                shortcut = crate::platform::primary_shortcut("⌘U", "Ctrl+U")
            )),
            (None, None) => SharedString::from(tr!(
                "usage.shortcut",
                shortcut = crate::platform::primary_shortcut("⌘U", "Ctrl+U")
            )),
        };

        let trigger = div()
            .id("usage-meter")
            .h(px(20.0))
            .px(px(5.0))
            .rounded(px(5.0))
            .flex()
            .items_center()
            .flex_none()
            .cursor_default()
            .hover(|element| element.bg(theme.overlay))
            .when(handle.is_open(), |element| element.bg(theme.overlay_strong))
            .tooltip(Tooltip::text(tooltip))
            .child(context_gauge(percent, theme.border_strong, fill));

        Some(popover(
            trigger,
            &handle,
            MenuAlign::AboveRight,
            move |handle, _, cx| {
                usage_panel(
                    handle,
                    provider,
                    context,
                    plan.clone(),
                    error.as_deref(),
                    plan_loading,
                    cx,
                )
            },
        ))
    }
}

fn context_percent(usage: ContextUsage) -> Option<f64> {
    usage
        .window
        .filter(|window| *window > 0)
        .map(|window| usage.tokens as f64 * 100.0 / window as f64)
}

/// The trigger glyph: a ring whose arc fills clockwise from 12 o'clock as the
/// context window does, over a faint full ring. An unknown fraction draws the
/// track alone. This is Zed's `CircularProgress` drawing sized for the footer
/// — `PathBuilder::stroke` arcs, which lyon tessellates correctly where a
/// hand-built annulus fill does not survive GPUI's fill rule.
fn context_gauge(percent: Option<f64>, track: Hsla, fill: Hsla) -> impl IntoElement {
    const SIZE: f32 = 13.0;
    const STROKE: f32 = 2.5;
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let center = bounds.center();
            let radius = px((SIZE - STROKE) / 2.0);

            // A full circle is two 180° arcs; lyon rejects a single
            // zero-length one.
            let full_circle = |builder: &mut PathBuilder| {
                builder.move_to(point(center.x + radius, center.y));
                builder.arc_to(
                    point(radius, radius),
                    px(0.0),
                    false,
                    true,
                    point(center.x - radius, center.y),
                );
                builder.arc_to(
                    point(radius, radius),
                    px(0.0),
                    false,
                    true,
                    point(center.x + radius, center.y),
                );
                builder.close();
            };

            let mut track_builder = PathBuilder::stroke(px(STROKE));
            full_circle(&mut track_builder);
            if let Ok(path) = track_builder.build() {
                window.paint_path(path, track);
            }

            let Some(percent) = percent else {
                return;
            };
            // Keep a visible sliver for a nearly-empty context.
            let fraction = ((percent / 100.0) as f32).clamp(0.0, 1.0).max(0.05);
            let mut arc_builder = PathBuilder::stroke(px(STROKE));
            if fraction >= 0.999 {
                full_circle(&mut arc_builder);
            } else {
                let start = -std::f32::consts::FRAC_PI_2;
                let angle = start + fraction * std::f32::consts::TAU;
                arc_builder.move_to(point(center.x, center.y - radius));
                arc_builder.arc_to(
                    point(radius, radius),
                    px(0.0),
                    fraction > 0.5,
                    true,
                    point(
                        center.x + radius * angle.cos(),
                        center.y + radius * angle.sin(),
                    ),
                );
            }
            if let Ok(path) = arc_builder.build() {
                window.paint_path(path, fill);
            }
        },
    )
    .w(px(SIZE))
    .h(px(SIZE))
    .flex_none()
}

fn usage_panel(
    handle: &ContextMenuHandle,
    provider: ProviderKind,
    context: Option<ContextUsage>,
    plan: Option<PlanUsage>,
    error: Option<&str>,
    plan_loading: bool,
    cx: &App,
) -> AnyElement {
    let theme = Theme::current(cx);
    let now = unix_time() as i64;
    let mut panel = div()
        // Focused on open so the surrounding menu context sees `escape`.
        .track_focus(handle.focus_handle())
        .w(px(320.0))
        .p(px(14.0))
        .rounded(px(10.0))
        .border_1()
        .border_color(theme.border_strong)
        .bg(theme.raised)
        .shadow_lg()
        .flex()
        .flex_col()
        .gap(px(12.0))
        .text_size(px(12.0));

    // The context row always renders; a session with nothing measured yet
    // reads "0" over an empty track, exactly like the CLI's own panel.
    let usage = context.unwrap_or_default();
    let percent = context_percent(usage);
    let value = match (usage.window, percent) {
        (Some(window), Some(percent)) => format!(
            "{} / {} ({percent:.0}%)",
            format_tokens(usage.tokens),
            format_tokens(window)
        ),
        // The transport reports occupancy but not the window size.
        _ => format_tokens(usage.tokens),
    };
    panel = panel.child(
        div()
            .flex()
            .flex_col()
            .gap(px(7.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(
                        div()
                            .text_color(theme.text)
                            .child(tr!("usage.context_window")),
                    )
                    .child(div().flex_1())
                    .child(
                        div()
                            .text_size(px(11.0))
                            .text_color(theme.text_tertiary)
                            .child(SharedString::from(value)),
                    ),
            )
            .child(meter_bar(&theme, percent.unwrap_or(0.0))),
    );
    if plan.is_some() || error.is_some() || plan_loading {
        panel = panel.child(div().h(px(1.0)).flex_none().bg(theme.border));
    }

    if let Some(plan) = plan {
        let header = match &plan.plan_label {
            Some(label) => tr!("usage.plan_limits_named", plan = label),
            None => tr!("usage.plan_limits"),
        };
        let usage_url = match provider {
            ProviderKind::Claude => Some("https://claude.ai/settings/usage"),
            ProviderKind::Codex => Some("https://chatgpt.com/codex/settings/usage"),
            _ => None,
        };
        let header_row = div().flex().items_center().gap(px(6.0)).child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .truncate()
                .text_size(px(11.0))
                .text_color(theme.text_tertiary)
                .child(SharedString::from(header)),
        );
        panel = panel.child(match usage_url {
            Some(url) => header_row
                .id("plan-usage-link")
                .cursor_default()
                .hover(|element| element.opacity(0.8))
                .tooltip(Tooltip::text(tr!("usage.open_account_settings")))
                .on_click(move |_, _, cx| cx.open_url(url))
                .child(icon("icons/arrow-right.svg", 10.0, theme.text_tertiary))
                .into_any_element(),
            None => header_row.into_any_element(),
        });
        for window in &plan.windows {
            panel = panel.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(7.0))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .child(
                                // A long lane label gives way — truncated
                                // with an ellipsis — rather than pushing the
                                // reset time and percent past the card edge.
                                div()
                                    .flex_1()
                                    .min_w(px(0.0))
                                    .truncate()
                                    .text_color(theme.text)
                                    .child(SharedString::from(window.label.clone())),
                            )
                            .children(window.resets_at.map(|resets_at| {
                                div()
                                    .flex_none()
                                    .text_size(px(11.0))
                                    .text_color(theme.text_tertiary)
                                    .child(SharedString::from(reset_label(resets_at, now)))
                            }))
                            .child(
                                div()
                                    .flex_none()
                                    .text_size(px(11.5))
                                    .text_color(theme.text_secondary)
                                    .child(SharedString::from(format!("{:.0}%", window.percent))),
                            ),
                    )
                    .child(meter_bar(&theme, window.percent)),
            );
        }
    } else if plan_loading {
        panel = panel.child(plan_skeleton(&theme));
    } else if let Some(error) = error {
        panel = panel.child(
            div()
                .flex()
                .flex_col()
                .gap(px(4.0))
                .child(
                    div()
                        .text_size(px(11.0))
                        .text_color(theme.text_tertiary)
                        .child(tr!("usage.plan_limits")),
                )
                .child(
                    div()
                        .text_size(px(11.0))
                        .text_color(theme.text_secondary)
                        .child(SharedString::from(tr!("usage.unavailable", error = error))),
                ),
        );
    }

    panel.into_any_element()
}

/// Placeholder for the plan section while its first fetch is in flight:
/// a header bar and two quota rows, pulsing gently. `with_animation` honors
/// the system's reduce-motion setting on its own.
fn plan_skeleton(theme: &Theme) -> AnyElement {
    let theme = *theme;
    let bar = move |width: f32| {
        div()
            .h(px(9.0))
            .w(px(width))
            .flex_none()
            .rounded(px(4.5))
            .bg(theme.overlay_strong)
    };
    let row = move |label_width: f32, value_width: f32| {
        div()
            .flex()
            .flex_col()
            .gap(px(9.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .child(bar(label_width))
                    .child(div().flex_1())
                    .child(bar(value_width)),
            )
            .child(
                div()
                    .h(px(3.0))
                    .w_full()
                    .flex_none()
                    .rounded_full()
                    .bg(theme.overlay_strong),
            )
    };
    motion::pulse(Duration::from_millis(1400), move |phase| {
        div()
            .flex()
            .flex_col()
            .gap(px(12.0))
            .child(bar(132.0))
            .child(row(96.0, 64.0))
            .child(row(120.0, 64.0))
            .opacity(pulsating_between(0.45, 0.9)(phase))
            .into_any_element()
    })
    .every(2)
    .into_any_element()
}

/// A quota bar: full-width track, fill proportional to `percent`. A lane in
/// use keeps a visible sliver even under one percent.
fn meter_bar(theme: &Theme, percent: f64) -> Div {
    let fraction = (percent / 100.0).clamp(0.0, 1.0) as f32;
    let fraction = if fraction > 0.0 {
        fraction.max(0.015)
    } else {
        0.0
    };
    let fill = if percent >= 95.0 {
        theme.danger
    } else if percent >= 80.0 {
        theme.warning
    } else {
        theme.gauge
    };
    div()
        .h(px(3.0))
        .w_full()
        .flex_none()
        .rounded_full()
        .bg(theme.overlay_strong)
        .child(div().h_full().w(relative(fraction)).rounded_full().bg(fill))
}

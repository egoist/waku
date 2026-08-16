//! The settings Usage page: historical token and cost usage across provider
//! transcripts, mirroring T3 Code's usage dashboard — a windowed headline with
//! per-provider share bars, a layered daily chart, a metric strip, a
//! model/day breakdown, and cost quality. Data comes from
//! [`crate::usage_history`], scanned by the daemon; frames read only the
//! snapshot stored on the entity.

use std::path::Path;

use chrono::{Datelike, Local, NaiveDate};
use gpui::{PathBuilder, relative};

use super::*;
use crate::usage_history::{
    self, MONTHLY_WINDOW, MonthSlice, PricingStatus, ProjectSlice, ProviderDay, UsageHistory,
    UsageProvider, UsageWindow, WINDOW_CHOICES,
};

/// Rendered chart height, matching T3's `h-56` plot.
const CHART_HEIGHT: f32 = 224.0;
/// Sliver above the top gridline so a peak's 2px stroke is not shaved off.
const CHART_PLOT_TOP: f32 = 8.0;
const CHART_TICKS: usize = 4;
/// Width of the y-axis label gutter.
const CHART_GUTTER: f32 = 56.0;
/// Uniform height hint for the virtualized project rows, so the scrollbar
/// knows the total extent before rows are measured.
const USAGE_PROJECT_ROW_HEIGHT: f32 = 96.0;
/// A snapshot older than this rescans when the page is next opened.
const USAGE_RESCAN_AFTER: Duration = Duration::from_secs(120);
fn provider_kind(provider: UsageProvider) -> ProviderKind {
    match provider {
        UsageProvider::Claude => ProviderKind::Claude,
        UsageProvider::Codex => ProviderKind::Codex,
    }
}

impl Waku {
    /// Switch the settings view to `page`, warming the Usage scan when that
    /// is where the user is heading.
    pub(super) fn open_settings_page(&mut self, page: SettingsPage, cx: &mut Context<Self>) {
        // Secrets are revealed only for the current visit to the page. This
        // also masks the token again when the Daemon row is reselected.
        self.daemon_token_revealed = false;
        self.set_active_page(Some(ActivePage::Settings(page)), cx);
        // Each page starts at its own top; a scroll position carried over
        // from the previous page would land mid-content.
        self.settings_scroll.set_offset(gpui::Point::default());
        if page == SettingsPage::Usage {
            self.ensure_usage_history(false, cx);
        }
        if page == SettingsPage::Skills {
            self.ensure_skills_catalog(false, cx);
        }
        cx.notify();
    }

    /// The scan window the active view needs: the statement view always
    /// covers a year of calendar months; the daily and project views share
    /// the trailing-days selector.
    fn effective_usage_window(&self) -> UsageWindow {
        match self.usage_view {
            UsageViewMode::Monthly => MONTHLY_WINDOW,
            UsageViewMode::Daily | UsageViewMode::Projects => self.usage_window,
        }
    }

    /// Start a background transcript scan unless a current-enough snapshot
    /// (or an in-flight scan for the same window) already covers it. `force`
    /// is the refresh button. Results from superseded scans are discarded by
    /// generation, so a window change mid-scan cannot land stale data.
    pub(super) fn ensure_usage_history(&mut self, force: bool, cx: &mut Context<Self>) {
        let window = self.effective_usage_window();
        let satisfied = self
            .usage_history
            .as_ref()
            .is_some_and(|history| history.window == window)
            && self
                .usage_history_scanned_at
                .is_some_and(|scanned| scanned.elapsed() < USAGE_RESCAN_AFTER);
        // A scan for this window already inbound absorbs even a forced
        // refresh — it only just started reading the same files, and a
        // duplicate would burn a background pass to produce the same answer.
        if self.usage_history_pending_for == Some(window) {
            return;
        }
        if !force && satisfied {
            return;
        }
        self.usage_history_pending_for = Some(window);
        self.usage_history_generation += 1;
        let generation = self.usage_history_generation;
        let daemon = self.daemon.client();
        let project_roots: Vec<PathBuf> = self
            .state
            .projects
            .iter()
            .map(|project| project.path.clone())
            .collect();
        cx.spawn(async move |this, cx| {
            let history = cx
                .background_executor()
                .spawn(async move {
                    match daemon.request(
                        Uuid::nil(),
                        Uuid::nil(),
                        waku_client::Command::LoadUsageHistory {
                            window,
                            project_roots,
                        },
                    )? {
                        waku_client::ResponsePayload::UsageHistory { history } => Ok(history),
                        _ => anyhow::bail!("the daemon returned an invalid usage response"),
                    }
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.usage_history_generation != generation {
                    return;
                }
                this.usage_history_pending_for = None;
                // The day axis may have changed length; a stale index would
                // point at the wrong day.
                this.usage_chart_hover = None;
                match history {
                    Ok(history) => {
                        this.usage_history_scanned_at = Some(Instant::now());
                        this.usage_history = Some(history);
                    }
                    Err(error) => this.show_toast(error.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn set_usage_window(&mut self, window: UsageWindow, cx: &mut Context<Self>) {
        if self.usage_window == window {
            return;
        }
        self.usage_window = window;
        self.ensure_usage_history(false, cx);
        cx.notify();
    }

    fn set_usage_view(&mut self, view: UsageViewMode, cx: &mut Context<Self>) {
        if self.usage_view == view {
            return;
        }
        self.usage_view = view;
        // The statement view scans a different window; the others share one.
        self.ensure_usage_history(false, cx);
        cx.notify();
    }

    pub(super) fn render_usage_settings(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::current(cx);
        let pending = self.usage_history_pending_for.is_some();
        let expected = self.effective_usage_window();
        // A snapshot of the other shape (statement months vs trailing days)
        // must not masquerade as this view's data — a 30-day scan rendered as
        // a monthly statement would label partial months as whole ones. But
        // within a shape, the previous window keeps rendering while its
        // replacement scans: the range caption names what is actually shown,
        // and swapping to a spinner on every window click would blink away a
        // page that is still substantially right.
        let history = self.usage_history.as_ref().filter(|history| {
            matches!(
                (history.window, expected),
                (UsageWindow::TrailingDays(_), UsageWindow::TrailingDays(_))
                    | (UsageWindow::Months(_), UsageWindow::Months(_))
            )
        });
        let range = history
            .map(|history| (history.since_day, history.until_day))
            .unwrap_or_else(|| expected.bounds(Local::now().date_naive()));

        let mut page = div()
            .flex()
            .flex_col()
            .when(
                matches!(
                    self.usage_view,
                    UsageViewMode::Monthly | UsageViewMode::Projects
                ),
                |element| {
                    // These views' lists own scrolling, so the page fills
                    // the pane instead of growing it.
                    element.flex_1().min_h_0().pb(px(16.0))
                },
            )
            .child(self.render_usage_header(range, pending, &theme, cx));

        let Some(history) = history else {
            // First scan (or a window-shape switch) still in flight: a
            // skeleton in the incoming view's silhouette, so the swap to
            // data doesn't jump.
            return page
                .child(usage_skeleton(self.usage_view, &theme))
                .into_any_element();
        };

        if !history.errors.is_empty() || history.pricing == PricingStatus::Unavailable {
            page = page.child(usage_notices(history, &theme));
        }

        page = match self.usage_view {
            UsageViewMode::Daily => page
                .child(
                    div()
                        .mt(px(20.0))
                        .flex()
                        .items_start()
                        .gap(px(28.0))
                        .child(self.render_usage_summary(history, &theme, cx))
                        .child(self.render_usage_chart_column(history, &theme, cx)),
                )
                .child(usage_metric_strip(history, &theme))
                .child(
                    div()
                        .mt(px(24.0))
                        .flex()
                        .items_start()
                        .gap(px(32.0))
                        .child(self.render_usage_breakdown(history, &theme, cx))
                        .child(usage_quality_panel(history, &theme)),
                ),
            UsageViewMode::Monthly => page.child(usage_month_list(
                self,
                history,
                &theme,
                &self.usage_months_scroll,
                &self.usage_months_scrollbar,
                cx,
            )),
            UsageViewMode::Projects => page.child(self.render_usage_projects(history, &theme, cx)),
        };

        page.child(
            // What the numbers above are built from, so the totals are
            // auditable at a glance.
            div()
                .mt(px(18.0))
                .text_size(px(9.5))
                .text_color(theme.text_ghost)
                .child(SharedString::from(tr!(
                    "usage.scan_summary",
                    scanned = format_count(history.scanned_files as u64),
                    skipped = format_count(history.skipped_files as u64),
                    records = format_count(history.records),
                    seconds = format!("{:.1}", history.scan_duration.as_secs_f64())
                ))),
        )
        .into_any_element()
    }

    /// The range caption plus the view switcher, the window selector (when
    /// the view honors it), and the refresh control.
    fn render_usage_header(
        &self,
        range: (NaiveDate, NaiveDate),
        pending: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Div {
        let monthly = self.usage_view == UsageViewMode::Monthly;

        let mut view_options = div()
            .rounded(px(7.0))
            .border_1()
            .border_color(theme.border_strong)
            .flex()
            .overflow_hidden();
        for (view, label) in [
            (UsageViewMode::Daily, tr!("usage.daily")),
            (UsageViewMode::Monthly, tr!("usage.monthly")),
            (UsageViewMode::Projects, tr!("usage.projects")),
        ] {
            let selected = self.usage_view == view;
            view_options = view_options.child(
                div()
                    .id(SharedString::from(format!(
                        "usage-view-{}",
                        label.to_ascii_lowercase()
                    )))
                    .tab_index(0)
                    .focus_visible(|style| style.border_1().border_color(theme.accent))
                    .h(px(26.0))
                    .px(px(11.0))
                    .flex()
                    .items_center()
                    .cursor_default()
                    .text_size(px(10.5))
                    .text_color(if selected {
                        theme.text
                    } else {
                        theme.text_secondary
                    })
                    .when(selected, |element| element.bg(theme.overlay))
                    .when(!selected, |element| {
                        element.hover(|element| element.text_color(theme.text))
                    })
                    .child(label)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.set_usage_view(view, cx);
                    })),
            );
        }

        // The statement view fixes its own range, so the window selector
        // would be a dead control there.
        let window_selector = (!monthly).then(|| {
            let selected = self.usage_window;
            let weak = cx.entity().downgrade();
            let handle = self.menu_handle("usage-window-selector", cx);
            dropdown_menu(
                MenuChip::new("usage-window-selector")
                    .label(window_choice_label(selected))
                    .outlined()
                    // Heights here are border-box: the view switcher is its
                    // 26px options plus 1px of border each side, so every
                    // control in this row targets 28px total — and the
                    // chip's raised-card fill would read as a pill on the
                    // page surface.
                    .height(px(28.0))
                    .background(theme.surface)
                    .selected(handle.is_open())
                    .w(px(124.0))
                    .justify_between(),
                "usage-window-selector-menu",
                &handle,
                MenuAlign::BelowRight,
                move |_| {
                    WINDOW_CHOICES
                        .into_iter()
                        .map(|window| {
                            let weak = weak.clone();
                            MenuItem::new(window_choice_label(window), move |_, cx| {
                                let _ = weak.update(cx, |this, cx| {
                                    this.set_usage_window(window, cx);
                                });
                            })
                            .selected(window == selected)
                        })
                        .collect()
                },
            )
        });

        let refresh_glyph: AnyElement = if pending {
            motion::spin(icon("icons/loader-circle.svg", 12.0, theme.text_tertiary))
        } else {
            icon("icons/rotate-cw.svg", 12.0, theme.text_tertiary).into_any_element()
        };
        let refresh = div()
            .id("usage-refresh")
            .tab_index(0)
            .focus_visible(|style| style.border_color(theme.accent))
            .h(px(28.0))
            .px(px(8.0))
            .rounded(px(7.0))
            .border_1()
            .border_color(theme.border_strong)
            .flex()
            .items_center()
            .cursor_default()
            .hover(|element| element.bg(theme.overlay))
            .tooltip(Tooltip::text(if pending {
                tr!("usage.scanning")
            } else {
                tr!("usage.rescan")
            }))
            .child(refresh_glyph)
            .on_click(cx.listener(|this, _, _, cx| {
                this.ensure_usage_history(true, cx);
            }));

        let range_label = if monthly {
            tr!(
                "usage.range",
                start = format_month_short(range.0),
                end = format_month_short(range.1)
            )
        } else {
            tr!(
                "usage.range",
                start = format_day_short(range.0),
                end = format_day_short(range.1)
            )
        };

        div()
            .mt(px(6.0))
            .flex()
            .items_center()
            .gap(px(10.0))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(12.5))
                    .text_color(theme.text_secondary)
                    .child(SharedString::from(range_label)),
            )
            .child(view_options)
            .children(window_selector)
            .child(refresh)
    }

    /// The headline figure in the active metric plus one share bar per
    /// provider. The summary follows the chart toggle so the headline and the
    /// series always read the same units.
    fn render_usage_summary(
        &self,
        history: &UsageHistory,
        theme: &Theme,
        _cx: &mut Context<Self>,
    ) -> Div {
        let metric = self.usage_metric;
        let headline = match metric {
            UsageMetric::Cost => format!("{}*", format_usd(history.cost_usd)),
            UsageMetric::Tokens => format_tokens_compact(history.total_tokens as f64),
        };
        let caption = match metric {
            UsageMetric::Cost => tr!("usage.full_api_rate_note"),
            UsageMetric::Tokens => tr!(
                "usage.sessions_summary",
                count = format_count(history.sessions)
            ),
        };

        let mut column = div()
            .w(px(300.0))
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(18.0))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(3.0))
                    .child(
                        div()
                            .text_size(px(10.0))
                            .text_color(theme.text_tertiary)
                            .child(match metric {
                                UsageMetric::Cost => tr!("usage.raw_token_cost"),
                                UsageMetric::Tokens => tr!("usage.processed_tokens_upper"),
                            }),
                    )
                    .child(
                        div()
                            .text_size(px(30.0))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.text)
                            .child(SharedString::from(headline)),
                    )
                    .child(
                        div()
                            .text_size(px(10.5))
                            .text_color(theme.text_tertiary)
                            .child(SharedString::from(caption)),
                    ),
            );

        // Ranked by whatever the toggle is showing, so the bars always
        // descend.
        let mut providers = history.providers.clone();
        if metric == UsageMetric::Tokens {
            providers.sort_by(|a, b| b.total_tokens.cmp(&a.total_tokens));
        }
        for provider in &providers {
            let kind = provider_kind(provider.provider);
            let color = provider_color(theme, kind);
            let share = match metric {
                UsageMetric::Cost => provider.cost_share,
                UsageMetric::Tokens => provider.token_share,
            };
            let value = match metric {
                UsageMetric::Cost => format_usd(provider.cost_usd),
                UsageMetric::Tokens => format_tokens_compact(provider.total_tokens as f64),
            };
            let detail = match metric {
                UsageMetric::Cost => tr!(
                    "usage.cost_share",
                    share = format_percent(share),
                    tokens = format_tokens_compact(provider.total_tokens as f64)
                ),
                UsageMetric::Tokens => tr!(
                    "usage.token_share",
                    share = format_percent(share),
                    cost = format_usd(provider.cost_usd)
                ),
            };
            column = column.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .child(icon(provider_icon(kind), 14.0, color))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(px(12.5))
                                    .text_color(theme.text)
                                    .child(provider.provider.label()),
                            )
                            .child(
                                div()
                                    .text_size(px(12.5))
                                    .text_color(theme.text)
                                    .child(SharedString::from(value)),
                            ),
                    )
                    .child(
                        div()
                            .h(px(4.0))
                            .w_full()
                            .rounded_full()
                            .bg(theme.overlay_strong)
                            .child(
                                div()
                                    .h_full()
                                    .w(relative((share as f32).clamp(0.0, 1.0)))
                                    .rounded_full()
                                    .bg(color),
                            ),
                    )
                    .child(
                        div()
                            .text_size(px(10.5))
                            .text_color(theme.text_tertiary)
                            .child(SharedString::from(detail)),
                    ),
            );
        }
        if history.providers.is_empty() {
            column = column.child(
                div()
                    .text_size(px(11.5))
                    .text_color(theme.text_tertiary)
                    .child(tr!("usage.no_activity_window")),
            );
        }
        column
    }

    /// The chart header (title, metric toggle, legend), the layered daily
    /// chart, and its x-axis labels.
    fn render_usage_chart_column(
        &self,
        history: &UsageHistory,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Div {
        let metric = self.usage_metric;
        let mut toggle = div()
            .rounded(px(7.0))
            .border_1()
            .border_color(theme.border_strong)
            .flex()
            .overflow_hidden();
        for (option, label) in [
            (UsageMetric::Cost, tr!("usage.cost_upper")),
            (UsageMetric::Tokens, tr!("usage.tokens_upper")),
        ] {
            let selected = metric == option;
            toggle = toggle.child(
                div()
                    .id(SharedString::from(format!("usage-metric-{label}")))
                    .tab_index(0)
                    .focus_visible(|style| style.border_1().border_color(theme.accent))
                    .h(px(22.0))
                    .px(px(9.0))
                    .flex()
                    .items_center()
                    .cursor_default()
                    .text_size(px(9.0))
                    .text_color(if selected {
                        theme.text
                    } else {
                        theme.text_secondary
                    })
                    .when(selected, |element| element.bg(theme.overlay))
                    .when(!selected, |element| {
                        element.hover(|element| element.text_color(theme.text))
                    })
                    .child(label)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if this.usage_metric != option {
                            this.usage_metric = option;
                            cx.notify();
                        }
                    })),
            );
        }

        let mut legend = div().flex().items_center().gap(px(14.0));
        for provider in UsageProvider::ALL {
            let kind = provider_kind(provider);
            legend = legend.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(5.0))
                    .child(icon(provider_icon(kind), 12.0, provider_color(theme, kind)))
                    .child(
                        div()
                            .text_size(px(10.5))
                            .text_color(theme.text_secondary)
                            .child(provider.label()),
                    ),
            );
        }

        let days = usage_history::enumerate_days(history.since_day, history.until_day);
        div()
            .flex_1()
            .min_w(px(320.0))
            .flex()
            .flex_col()
            .gap(px(10.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.0))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(12.5))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.text)
                            .child(match metric {
                                UsageMetric::Cost => tr!("usage.daily_cost"),
                                UsageMetric::Tokens => tr!("usage.daily_processed_tokens"),
                            }),
                    )
                    .child(toggle)
                    .child(legend),
            )
            .child(self.render_usage_chart(history, &days, theme, cx))
            .child(
                div()
                    .pl(px(CHART_GUTTER + 8.0))
                    .flex()
                    .justify_between()
                    .text_size(px(9.5))
                    .text_color(theme.text_tertiary)
                    .child(SharedString::from(
                        days.first()
                            .copied()
                            .map(format_day_short)
                            .unwrap_or_default(),
                    ))
                    .child(SharedString::from(
                        days.get(days.len() / 2)
                            .copied()
                            .map(format_day_short)
                            .unwrap_or_default(),
                    ))
                    .child(SharedString::from(
                        days.last()
                            .copied()
                            .map(format_day_short)
                            .unwrap_or_default(),
                    )),
            )
    }

    /// The plot: y-axis gutter, layered per-provider curves, and the hover
    /// readout. Values are absolute, not cumulative — the series are layered
    /// from a shared zero baseline rather than stacked, because a stacked
    /// chart puts whichever provider is drawn last permanently above the
    /// other, which reads as "that one is bigger" even on days it is not.
    fn render_usage_chart(
        &self,
        history: &UsageHistory,
        days: &[NaiveDate],
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Div {
        let metric = self.usage_metric;
        let day_count = days.len();
        // One column per day, per provider in ALL order. The chart paths and
        // the hover readout both consume this, so the number under the cursor
        // is by construction the number that was plotted.
        let series: Vec<[f64; 2]> = days
            .iter()
            .map(|day| {
                let slice = history.day(*day);
                let value = |provider: UsageProvider| {
                    slice
                        .map(|slice| {
                            let entry = slice.by_provider[provider.index()];
                            match metric {
                                UsageMetric::Cost => entry.cost_usd,
                                UsageMetric::Tokens => entry.total_tokens as f64,
                            }
                        })
                        .unwrap_or(0.0)
                };
                [value(UsageProvider::Claude), value(UsageProvider::Codex)]
            })
            .collect();
        // The scale tops out at the largest single provider-day, not the
        // largest sum: layered series each measure from zero, so a combined
        // peak would leave the plot permanently half empty.
        let peak = series
            .iter()
            .flat_map(|bands| bands.iter().copied())
            .fold(0.0_f64, f64::max);
        let (scale_max, ticks) = nice_scale(peak, CHART_TICKS);
        let format_value = move |value: f64| match metric {
            UsageMetric::Cost => format_usd(value),
            UsageMetric::Tokens => format_tokens_compact(value),
        };
        // Fraction of the plot height for a value, shared by the canvas, the
        // gutter labels, and nothing else.
        let to_fraction = move |value: f64| -> f32 {
            if scale_max <= 0.0 {
                1.0
            } else {
                1.0 - (value / scale_max) as f32 * (1.0 - CHART_PLOT_TOP / CHART_HEIGHT)
            }
        };

        let mut gutter = div()
            .relative()
            .w(px(CHART_GUTTER))
            .h(px(CHART_HEIGHT))
            .flex_none();
        for tick in &ticks {
            gutter = gutter.child(
                div()
                    .absolute()
                    .right(px(0.0))
                    .top(px((to_fraction(*tick) * CHART_HEIGHT - 7.0).max(0.0)))
                    .text_size(px(9.5))
                    .text_color(theme.text_tertiary)
                    .child(SharedString::from(if *tick == 0.0 {
                        "0".to_owned()
                    } else {
                        format_value(*tick)
                    })),
            );
        }

        let hover = self.usage_chart_hover.filter(|index| *index < day_count);
        let colors = [
            provider_color(theme, ProviderKind::Claude),
            provider_color(theme, ProviderKind::Codex),
        ];
        let bounds_cell = self.usage_chart_bounds.clone();
        let paint_series = series.clone();
        let paint_ticks = ticks.clone();
        let grid_color = theme.border;
        let hover_color = theme.text_ghost;
        let plot_canvas = canvas(
            |_, _, _| (),
            move |bounds, _, window, _| {
                bounds_cell.set(Some(bounds));
                let width = f32::from(bounds.size.width);
                let height = f32::from(bounds.size.height);
                let to_y = |value: f64| bounds.origin.y + px(to_fraction(value) * height);
                for tick in &paint_ticks {
                    window.paint_quad(fill(
                        gpui::Bounds::new(
                            point(bounds.origin.x, to_y(*tick)),
                            gpui::size(bounds.size.width, px(1.0)),
                        ),
                        grid_color,
                    ));
                }
                if paint_series.is_empty() {
                    return;
                }

                let step = if paint_series.len() <= 1 {
                    0.0
                } else {
                    width / (paint_series.len() - 1) as f32
                };
                let mut layers: Vec<(usize, f64)> = (0..colors.len())
                    .map(|provider| {
                        (
                            provider,
                            paint_series
                                .iter()
                                .map(|bands| bands[provider])
                                .sum::<f64>(),
                        )
                    })
                    .collect();
                // Paint the heavier series' fill first so the lighter one is
                // never buried under it; the strokes are drawn in a second
                // pass regardless, so neither can be hidden.
                layers.sort_by(|a, b| b.1.total_cmp(&a.1));

                let curves: Vec<(usize, Vec<CurveSegment>)> = layers
                    .iter()
                    .map(|(provider, _)| {
                        let points: Vec<(f32, f32)> = paint_series
                            .iter()
                            .enumerate()
                            .map(|(index, bands)| {
                                (
                                    f32::from(bounds.origin.x) + index as f32 * step,
                                    f32::from(to_y(bands[*provider])),
                                )
                            })
                            .collect();
                        (*provider, smooth_curve(&points))
                    })
                    .collect();

                let bottom = bounds.origin.y + bounds.size.height;
                for (provider, segments) in &curves {
                    let Some(first) = segments.first() else {
                        continue;
                    };
                    let mut area = PathBuilder::fill();
                    area.move_to(point(px(first.from.0), px(first.from.1)));
                    for segment in segments {
                        area.cubic_bezier_to(
                            point(px(segment.to.0), px(segment.to.1)),
                            point(px(segment.c1.0), px(segment.c1.1)),
                            point(px(segment.c2.0), px(segment.c2.1)),
                        );
                    }
                    area.line_to(point(bounds.origin.x + bounds.size.width, bottom));
                    area.line_to(point(bounds.origin.x, bottom));
                    area.close();
                    if let Ok(path) = area.build() {
                        window.paint_path(path, colors[*provider].opacity(0.12));
                    }
                }
                for (provider, segments) in &curves {
                    let Some(first) = segments.first() else {
                        continue;
                    };
                    let mut line = PathBuilder::stroke(px(2.0));
                    line.move_to(point(px(first.from.0), px(first.from.1)));
                    for segment in segments {
                        line.cubic_bezier_to(
                            point(px(segment.to.0), px(segment.to.1)),
                            point(px(segment.c1.0), px(segment.c1.1)),
                            point(px(segment.c2.0), px(segment.c2.1)),
                        );
                    }
                    if let Ok(path) = line.build() {
                        window.paint_path(path, colors[*provider]);
                    }
                }

                if let Some(index) = hover {
                    let x = bounds.origin.x + px(index as f32 * step);
                    window.paint_quad(fill(
                        gpui::Bounds::new(
                            point(x, bounds.origin.y + px(CHART_PLOT_TOP)),
                            gpui::size(px(1.0), bounds.size.height - px(CHART_PLOT_TOP)),
                        ),
                        hover_color,
                    ));
                }
            },
        );

        let plot = div()
            .id("usage-chart-plot")
            .relative()
            .flex_1()
            .min_w(px(0.0))
            .h(px(CHART_HEIGHT))
            .tab_index(0)
            .focus_visible(|style| style.border_1().border_color(theme.accent))
            .on_mouse_move(cx.listener(move |this, event: &MouseMoveEvent, _, cx| {
                let Some(bounds) = this.usage_chart_bounds.get() else {
                    return;
                };
                if day_count == 0 || f32::from(bounds.size.width) <= 0.0 {
                    return;
                }
                let fraction =
                    ((event.position.x - bounds.origin.x) / bounds.size.width).clamp(0.0, 1.0);
                let index = ((fraction * day_count.saturating_sub(1) as f32).round() as usize)
                    .min(day_count - 1);
                if this.usage_chart_hover != Some(index) {
                    this.usage_chart_hover = Some(index);
                    cx.notify();
                }
            }))
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                if !hovered && this.usage_chart_hover.is_some() {
                    this.usage_chart_hover = None;
                    cx.notify();
                }
            }))
            // The hover readout is also keyboard reachable: focus the plot
            // and step days with the arrows.
            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, _, cx| {
                if day_count == 0 {
                    return;
                }
                let last = day_count - 1;
                let next = match event.keystroke.key.as_str() {
                    "left" => Some(
                        this.usage_chart_hover
                            .map_or(last, |index| index.saturating_sub(1)),
                    ),
                    "right" => Some(
                        this.usage_chart_hover
                            .map_or(0, |index| (index + 1).min(last)),
                    ),
                    "home" => Some(0),
                    "end" => Some(last),
                    "escape" if this.usage_chart_hover.is_some() => None,
                    _ => return,
                };
                cx.stop_propagation();
                if this.usage_chart_hover != next {
                    this.usage_chart_hover = next;
                    cx.notify();
                }
            }))
            .child(plot_canvas.size_full())
            .when_some(
                hover.and_then(|index| days.get(index).map(|day| (index, *day))),
                |element, (index, day)| {
                    element.child(usage_chart_readout(
                        history,
                        day,
                        if day_count <= 1 {
                            0.0
                        } else {
                            index as f32 / (day_count - 1) as f32
                        },
                        metric,
                        theme,
                    ))
                },
            );

        div().flex().gap(px(8.0)).child(gutter).child(plot)
    }

    /// The breakdown table with its model/day toggle.
    fn render_usage_breakdown(
        &self,
        history: &UsageHistory,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Div {
        let breakdown = self.usage_breakdown;
        let mut toggle = div()
            .rounded(px(7.0))
            .border_1()
            .border_color(theme.border_strong)
            .flex()
            .overflow_hidden();
        for (option, label) in [
            (UsageBreakdown::Model, tr!("usage.model_upper")),
            (UsageBreakdown::Day, tr!("usage.day_upper")),
        ] {
            let selected = breakdown == option;
            toggle = toggle.child(
                div()
                    .id(SharedString::from(format!("usage-breakdown-{label}")))
                    .tab_index(0)
                    .focus_visible(|style| style.border_1().border_color(theme.accent))
                    .h(px(22.0))
                    .px(px(9.0))
                    .flex()
                    .items_center()
                    .cursor_default()
                    .text_size(px(9.0))
                    .text_color(if selected {
                        theme.text
                    } else {
                        theme.text_secondary
                    })
                    .when(selected, |element| element.bg(theme.overlay))
                    .when(!selected, |element| {
                        element.hover(|element| element.text_color(theme.text))
                    })
                    .child(label)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if this.usage_breakdown != option {
                            this.usage_breakdown = option;
                            cx.notify();
                        }
                    })),
            );
        }

        div()
            .flex_1()
            .min_w(px(0.0))
            .flex()
            .flex_col()
            .gap(px(10.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.0))
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(12.5))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.text)
                            .child(tr!("usage.breakdown")),
                    )
                    .child(toggle),
            )
            .child(match breakdown {
                UsageBreakdown::Model => usage_model_table(history, theme),
                UsageBreakdown::Day => usage_day_table(history, theme),
            })
    }

    /// The per-project ranking: one row per working directory the sessions
    /// ran in, largest first, with the same split-bar vocabulary as the
    /// monthly statement. The rows live in a virtualized `list()` behind a
    /// filter field, so element construction stays proportional to what is
    /// on screen no matter how many directories have usage.
    fn render_usage_projects(
        &self,
        history: &UsageHistory,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Div {
        let by_cost = rank_by_cost(history);
        let filter = self
            .usage_project_filter
            .read(cx)
            .content()
            .trim()
            .to_ascii_lowercase();
        let indices: Vec<usize> = history
            .projects
            .iter()
            .enumerate()
            .filter(|(_, project)| {
                if filter.is_empty() {
                    return true;
                }
                let (name, _) = self.usage_project_identity(project);
                name.to_ascii_lowercase().contains(&filter)
                    || project.path.to_ascii_lowercase().contains(&filter)
            })
            .map(|(index, _)| index)
            .collect();
        // The bars scale against the largest visible row; the builder reads
        // this instead of re-deriving it per row.
        let peak = indices
            .iter()
            .filter_map(|index| history.projects.get(*index))
            .map(|project| {
                if by_cost {
                    project.cost_usd
                } else {
                    project.total_tokens as f64
                }
            })
            .fold(0.0_f64, f64::max);
        self.usage_projects_scale.set((peak, by_cost));
        self.sync_usage_project_rows(&indices);

        let caption = if filter.is_empty() {
            tr!(
                "usage.projects_caption",
                projects = count_noun(history.projects.len() as u64, "project"),
                tokens = format_tokens_compact(history.total_tokens as f64),
                sessions = count_noun(history.sessions, "session")
            )
        } else {
            tr!(
                "usage.projects_shown",
                shown = indices.len(),
                projects = count_noun(history.projects.len() as u64, "project")
            )
        };

        let entity = cx.entity().downgrade();
        let body: AnyElement = if history.projects.is_empty() {
            div()
                .px(px(20.0))
                .child(usage_list_empty_row(theme, tr!("usage.no_activity_window")))
                .into_any_element()
        } else if indices.is_empty() {
            div()
                .px(px(20.0))
                .child(usage_list_empty_row(theme, tr!("usage.no_projects_match")))
                .into_any_element()
        } else {
            // The relative wrapper is full-bleed so the overlay scrollbar
            // pins to the card's edge; the content padding lives one level
            // in, the same way the sidebar hangs its scrollbar.
            div()
                .flex_1()
                .min_h_0()
                .relative()
                .child(
                    div().px(px(20.0)).size_full().child(
                        list(
                            self.usage_projects_list.clone(),
                            move |index, _window, cx| {
                                entity
                                    .upgrade()
                                    .map(|entity| {
                                        entity.update(cx, |this, cx| {
                                            this.usage_project_row(index, cx)
                                        })
                                    })
                                    .unwrap_or_else(|| div().into_any_element())
                            },
                        )
                        .size_full(),
                    ),
                )
                .child(scrollbar::vertical(
                    &self.usage_projects_list,
                    &self.usage_projects_scrollbar,
                ))
                .into_any_element()
        };

        div()
            .mt(px(20.0))
            .flex_1()
            .min_h_0()
            .pb(px(8.0))
            .rounded(px(13.0))
            .bg(theme.raised)
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_none()
                    .px(px(20.0))
                    .py(px(13.0))
                    .border_b_1()
                    .border_color(theme.border)
                    .flex()
                    .items_center()
                    .gap(px(16.0))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .text_size(px(13.5))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme.text)
                                    .child(tr!("usage.by_project")),
                            )
                            .child(
                                div()
                                    .mt(px(3.0))
                                    .flex()
                                    .items_baseline()
                                    .gap(px(6.0))
                                    .child(
                                        div()
                                            .flex_none()
                                            .text_size(px(11.5))
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(theme.text)
                                            .child(SharedString::from(usage_headline_value(
                                                history, by_cost,
                                            ))),
                                    )
                                    .child(
                                        div()
                                            .min_w_0()
                                            .truncate()
                                            .text_size(px(10.5))
                                            .text_color(theme.text_tertiary)
                                            .child(SharedString::from(format!("· {caption}"))),
                                    ),
                            ),
                    )
                    .child(
                        TextField::new("usage-project-filter", self.usage_project_filter.clone())
                            .icon("icons/search.svg", 13.0)
                            .w(px(240.0))
                            .flex_none(),
                    ),
            )
            .child(body)
    }

    /// Keep the virtualized project list in sync with the filtered indices.
    /// Filtering preserves order, so unrelated churn splices only the
    /// changed suffix and scroll position survives typing in the filter.
    fn sync_usage_project_rows(&self, indices: &[usize]) {
        let mut cached = self.usage_projects_rows.borrow_mut();
        if cached.as_slice() == indices {
            return;
        }
        let prefix = cached
            .iter()
            .zip(indices.iter())
            .take_while(|(cached, fresh)| cached == fresh)
            .count();
        let old_count = cached.len();
        *cached = indices.to_vec();
        if old_count == 0 {
            self.usage_projects_list
                .reset_with_uniform_height(indices.len(), px(USAGE_PROJECT_ROW_HEIGHT));
        } else {
            self.usage_projects_list
                .splice(prefix..old_count, indices.len() - prefix);
            // Newly inserted rows have no measured height yet; the uniform
            // hint keeps the scrollbar's total height honest.
            self.usage_projects_list
                .clone()
                .with_uniform_item_height(px(USAGE_PROJECT_ROW_HEIGHT));
        }
    }

    /// One project row, built only while visible. Reads the per-frame row
    /// cache and scale; a stale index from a frame racing a rescan renders
    /// as an empty row for that frame rather than panicking.
    fn usage_project_row(&self, row: usize, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::current(cx);
        let (peak, by_cost) = self.usage_projects_scale.get();
        let colors = usage_provider_colors(&theme);
        let rows = self.usage_projects_rows.borrow();
        let last = row + 1 == rows.len();
        let Some(index) = rows.get(row).copied() else {
            return div().into_any_element();
        };
        let Some(project) = self
            .usage_history
            .as_ref()
            .and_then(|history| history.projects.get(index))
        else {
            return div().into_any_element();
        };

        let (name, path_caption) = self.usage_project_identity(project);
        let row_value = if by_cost {
            project.cost_usd
        } else {
            project.total_tokens as f64
        };
        let mut caption_parts = Vec::new();
        if by_cost && project.cost_share > 0.0 {
            caption_parts.push(tr!(
                "usage.percent_of_cost",
                share = format_percent(project.cost_share)
            ));
        }
        caption_parts.push(tr!(
            "usage.token_count",
            count = format_tokens_compact(project.total_tokens as f64)
        ));
        caption_parts.push(count_noun(project.sessions, "session"));
        if let Some(last_day) = project.last_day {
            caption_parts.push(tr!("usage.last_active", date = format_day_short(last_day)));
        }
        let models_control = self.usage_models_control(
            format!(
                "usage-project-models-{}-{index}",
                self.usage_history_generation
            ),
            &project.top_models,
            project.cost_usd,
            &theme,
            cx,
        );

        div()
            // The list lays items out at their content size; without an
            // explicit width the row and its bars shrink-wrap the text.
            .w_full()
            .py(px(13.0))
            .when(!last, |element| {
                element.border_b_1().border_color(theme.border)
            })
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .items_baseline()
                    .gap(px(8.0))
                    .child(
                        div()
                            .text_size(px(13.0))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.text)
                            .child(SharedString::from(name)),
                    )
                    .when_some(path_caption, |element, path| {
                        element.child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_size(px(9.5))
                                .text_color(theme.text_ghost)
                                .child(SharedString::from(path)),
                        )
                    })
                    .child(div().flex_1())
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(14.0))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.text)
                            .child(SharedString::from(usage_value_label(
                                project.cost_usd,
                                project.total_tokens,
                                by_cost,
                            ))),
                    ),
            )
            .child(
                div()
                    .mt(px(2.0))
                    .text_size(px(10.5))
                    .text_color(theme.text_tertiary)
                    .truncate()
                    .child(SharedString::from(caption_parts.join(" · "))),
            )
            .child(div().mt(px(9.0)).child(usage_split_bar(
                &theme,
                colors,
                if peak <= 0.0 {
                    0.0
                } else {
                    (row_value / peak) as f32
                },
                &project.by_provider,
                by_cost,
            )))
            .child(
                div()
                    .mt(px(7.0))
                    .flex()
                    .items_center()
                    .gap(px(12.0))
                    .child(usage_provider_values(&theme, &project.by_provider, by_cost))
                    .child(div().flex_1())
                    .when_some(models_control, |element, control| element.child(control)),
            )
            .into_any_element()
    }

    /// A compact model summary that opens a lazy, keyboard-operable detail
    /// menu. Closed rows retain no cloned model list; only the open row builds
    /// the detail data and card.
    fn usage_models_control(
        &self,
        id: impl Into<SharedString>,
        top_models: &[(String, f64)],
        total_cost: f64,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let label = usage_top_models_label(top_models)?;
        let model_count = top_models.len() as u64;
        let id = id.into();
        let handle = self.menu_handle(id.clone(), cx);
        let open_models = handle.is_open().then(|| Rc::new(top_models.to_vec()));
        let trigger = div()
            .id(SharedString::from(format!("{id}-trigger")))
            .h(px(22.0))
            .max_w(px(300.0))
            .px(px(6.0))
            .rounded(px(5.0))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(5.0))
            .cursor_default()
            .text_size(px(9.5))
            .text_color(theme.text_tertiary)
            .focus_visible(|style| style.border_1().border_color(theme.accent))
            .when(handle.is_open(), |element| element.bg(theme.overlay_strong))
            .hover(|element| element.bg(theme.overlay))
            .tooltip(Tooltip::text(SharedString::from(tr!(
                "usage.show_models",
                models = count_noun(model_count, "model")
            ))))
            .child(div().min_w_0().truncate().child(SharedString::from(label)))
            .child(icon("icons/chevron-down.svg", 8.0, theme.text_ghost));

        Some(dropdown_menu(
            trigger,
            SharedString::from(format!("{id}-popover")),
            &handle,
            MenuAlign::BelowRight,
            move |_| {
                open_models
                    .as_ref()
                    .map(|models| usage_models_menu_items(models.clone(), total_cost))
                    .unwrap_or_default()
            },
        ))
    }

    /// Display name and path caption for a project row: a known Waku
    /// project's name when the path is one, else the directory's own name
    /// alongside its complete path, shortening only the home prefix.
    fn usage_project_identity(&self, project: &ProjectSlice) -> (String, Option<String>) {
        if project.path.is_empty() {
            return (tr!("usage.other_sessions"), None);
        }
        let path = Path::new(&project.path);
        let name = self
            .state
            .projects
            .iter()
            .find(|known| known.path.as_path() == path)
            .map(|known| known.name.clone())
            .filter(|name| !name.is_empty())
            .or_else(|| {
                path.file_name()
                    .map(|name| name.to_string_lossy().into_owned())
            })
            .unwrap_or_else(|| project.path.clone());
        let home = crate::projectless::home_directory();
        (name, Some(usage_project_path(path, home.as_deref())))
    }
}

/* ------------------------------------------------------------------------- */
/* Stateless sections                                                        */
/* ------------------------------------------------------------------------- */

/// Says plainly when the totals are incomplete: an unreadable transcript
/// directory, or no rate table to price against.
fn usage_notices(history: &UsageHistory, theme: &Theme) -> Div {
    let mut notice = div()
        .mt(px(14.0))
        .px(px(12.0))
        .py(px(8.0))
        .rounded(px(8.0))
        .border_1()
        .border_color(theme.border)
        .flex()
        .flex_col()
        .gap(px(3.0))
        .text_size(px(10.5))
        .text_color(theme.text_tertiary);
    for error in &history.errors {
        notice = notice.child(SharedString::from(error.clone()));
    }
    if history.pricing == PricingStatus::Unavailable {
        notice = notice.child(tr!("usage.rates_unavailable"));
    }
    notice
}

/// One day's per-provider values under the cursor, anchored to the hovered
/// column and flipped near the right edge so it stays inside the plot.
fn usage_chart_readout(
    history: &UsageHistory,
    day: NaiveDate,
    fraction: f32,
    metric: UsageMetric,
    theme: &Theme,
) -> Div {
    let slice = history.day(day);
    let value = |provider: UsageProvider| {
        slice
            .map(|slice| {
                let entry = slice.by_provider[provider.index()];
                match metric {
                    UsageMetric::Cost => entry.cost_usd,
                    UsageMetric::Tokens => entry.total_tokens as f64,
                }
            })
            .unwrap_or(0.0)
    };
    let format_value = |value: f64| match metric {
        UsageMetric::Cost => format_usd(value),
        UsageMetric::Tokens => format_tokens_compact(value),
    };

    let mut readout = div()
        .absolute()
        .top(px(0.0))
        .when(fraction <= 0.6, |element| element.left(relative(fraction)))
        .when(fraction > 0.6, |element| {
            element.right(relative(1.0 - fraction))
        })
        .min_w(px(150.0))
        .px(px(9.0))
        .py(px(7.0))
        .rounded(px(8.0))
        .border_1()
        .border_color(theme.border_strong)
        .bg(theme.raised)
        .shadow_md()
        .flex()
        .flex_col()
        .gap(px(3.0))
        .text_size(px(10.5))
        .child(
            div()
                .text_color(theme.text_tertiary)
                .child(SharedString::from(format_day_short(day))),
        );
    let mut total = 0.0;
    for provider in UsageProvider::ALL {
        let kind = provider_kind(provider);
        let amount = value(provider);
        total += amount;
        readout = readout.child(
            div()
                .flex()
                .items_center()
                .gap(px(10.0))
                .child(icon(provider_icon(kind), 11.0, provider_color(theme, kind)))
                .child(
                    div()
                        .flex_1()
                        .text_color(theme.text_secondary)
                        .child(provider.label()),
                )
                .child(
                    div()
                        .text_color(theme.text)
                        .child(SharedString::from(format_value(amount))),
                ),
        );
    }
    readout.child(
        div()
            .mt(px(2.0))
            .pt(px(4.0))
            .border_t_1()
            .border_color(theme.border)
            .flex()
            .items_center()
            .gap(px(10.0))
            .child(
                div()
                    .flex_1()
                    .text_color(theme.text_secondary)
                    .child(tr!("usage.total")),
            )
            .child(
                div()
                    .text_color(theme.text)
                    .child(SharedString::from(format_value(total))),
            ),
    )
}

/// The five-figure strip under the chart: token mix and cache economics.
fn usage_metric_strip(history: &UsageHistory, theme: &Theme) -> Div {
    let active_days = history
        .daily
        .iter()
        .filter(|day| day.total_tokens > 0)
        .count();
    let daily_average = if active_days == 0 {
        0.0
    } else {
        history.total_tokens as f64 / active_days as f64
    };
    let observed_input = history.totals.uncached_input + history.totals.cached_input;
    let cached_share = if observed_input == 0 {
        0.0
    } else {
        history.totals.cached_input as f64 / observed_input as f64
    };
    let savings_detail = if history.cost_usd > 0.0 {
        tr!(
            "usage.raw_cost_multiple",
            multiple = format!(
                "{:.1}",
                history.quality.cache_savings_usd / history.cost_usd
            )
        )
    } else {
        tr!("usage.vs_full_input_rates")
    };

    let tiles: [(String, String, String); 5] = [
        (
            tr!("usage.processed_tokens"),
            format_tokens_compact(history.total_tokens as f64),
            tr!(
                "usage.per_active_day",
                count = format_tokens_compact(daily_average)
            ),
        ),
        (
            tr!("usage.cached_input"),
            format_tokens_compact(history.totals.cached_input as f64),
            tr!(
                "usage.observed_input_share",
                share = format_percent(cached_share)
            ),
        ),
        (
            tr!("usage.uncached_input"),
            format_tokens_compact(history.totals.uncached_input as f64),
            tr!(
                "usage.cache_writes",
                count = format_tokens_compact(history.totals.cache_creation as f64)
            ),
        ),
        (
            tr!("usage.output"),
            format_tokens_compact(history.totals.output as f64),
            tr!(
                "usage.includes_reasoning",
                count = format_tokens_compact(history.totals.reasoning as f64)
            ),
        ),
        (
            tr!("usage.cache_savings"),
            format_usd(history.quality.cache_savings_usd),
            savings_detail,
        ),
    ];

    let mut strip = div()
        .mt(px(24.0))
        .border_t_1()
        .border_b_1()
        .border_color(theme.border)
        .flex();
    for (index, (label, value, detail)) in tiles.into_iter().enumerate() {
        strip = strip.child(
            div()
                .flex_1()
                .min_w_0()
                .px(px(14.0))
                .py(px(11.0))
                .when(index > 0, |element| {
                    element.border_l_1().border_color(theme.border)
                })
                .flex()
                .flex_col()
                .gap(px(2.0))
                .child(
                    div()
                        .text_size(px(10.0))
                        .text_color(theme.text_tertiary)
                        .truncate()
                        .child(label),
                )
                .child(
                    div()
                        .text_size(px(15.0))
                        .text_color(theme.text)
                        .truncate()
                        .child(SharedString::from(value)),
                )
                .child(
                    div()
                        .text_size(px(9.5))
                        .text_color(theme.text_tertiary)
                        .truncate()
                        .child(SharedString::from(detail)),
                ),
        );
    }
    strip
}

fn usage_table_empty_row(theme: &Theme) -> Div {
    div()
        .py(px(24.0))
        .flex()
        .justify_center()
        .text_size(px(11.5))
        .text_color(theme.text_tertiary)
        .child(tr!("usage.no_activity_window"))
}

/// Right-aligned numeric cell of fixed width.
fn usage_cell(width: f32, text: String, color: Hsla) -> Div {
    div()
        .w(px(width))
        .flex_none()
        .flex()
        .justify_end()
        .text_color(color)
        .child(SharedString::from(text))
}

/// Per-model costs, largest first.
fn usage_model_table(history: &UsageHistory, theme: &Theme) -> Div {
    let mut table = div().flex().flex_col().text_size(px(11.5)).child(
        div()
            .pb(px(7.0))
            .border_b_1()
            .border_color(theme.border)
            .flex()
            .items_center()
            .gap(px(12.0))
            .text_size(px(10.5))
            .text_color(theme.text_tertiary)
            .child(div().flex_1().min_w_0().child(tr!("usage.model")))
            .child(usage_cell(84.0, tr!("usage.cost"), theme.text_tertiary))
            .child(usage_cell(64.0, tr!("usage.share"), theme.text_tertiary))
            .child(usage_cell(84.0, tr!("usage.tokens"), theme.text_tertiary)),
    );
    if history.models.is_empty() {
        return table.child(usage_table_empty_row(theme));
    }
    for model in &history.models {
        let kind = provider_kind(model.provider);
        table = table.child(
            div()
                .py(px(8.0))
                .border_b_1()
                .border_color(theme.border)
                .flex()
                .items_center()
                .gap(px(12.0))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .items_center()
                        .gap(px(7.0))
                        .child(icon(provider_icon(kind), 12.0, provider_color(theme, kind)))
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_color(theme.text)
                                .child(SharedString::from(model.model.clone())),
                        ),
                )
                .child(usage_cell(84.0, format_usd(model.cost_usd), theme.text))
                .child(usage_cell(
                    64.0,
                    format_percent(model.cost_share),
                    theme.text_tertiary,
                ))
                .child(usage_cell(
                    84.0,
                    format_tokens_compact(model.total_tokens as f64),
                    theme.text_tertiary,
                )),
        );
    }
    table
}

/// The most recent active days, newest first, with per-provider cost columns.
fn usage_day_table(history: &UsageHistory, theme: &Theme) -> Div {
    let mut header = div()
        .pb(px(7.0))
        .border_b_1()
        .border_color(theme.border)
        .flex()
        .items_center()
        .gap(px(12.0))
        .text_size(px(10.5))
        .text_color(theme.text_tertiary)
        .child(div().flex_1().min_w_0().child(tr!("usage.day")));
    for provider in UsageProvider::ALL {
        header = header.child(usage_cell(
            84.0,
            provider.label().to_owned(),
            theme.text_tertiary,
        ));
    }
    header = header
        .child(usage_cell(84.0, tr!("usage.total"), theme.text_tertiary))
        .child(usage_cell(84.0, tr!("usage.tokens"), theme.text_tertiary));

    let mut table = div().flex().flex_col().text_size(px(11.5)).child(header);
    if history.daily.is_empty() {
        return table.child(usage_table_empty_row(theme));
    }
    for day in history.daily.iter().rev().take(8) {
        let mut row = div()
            .py(px(8.0))
            .border_b_1()
            .border_color(theme.border)
            .flex()
            .items_center()
            .gap(px(12.0))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_color(theme.text)
                    .child(SharedString::from(format_day_short(day.day))),
            );
        for provider in UsageProvider::ALL {
            row = row.child(usage_cell(
                84.0,
                format_usd(day.by_provider[provider.index()].cost_usd),
                theme.text_tertiary,
            ));
        }
        table = table.child(
            row.child(usage_cell(84.0, format_usd(day.cost_usd), theme.text))
                .child(usage_cell(
                    84.0,
                    format_tokens_compact(day.total_tokens as f64),
                    theme.text_tertiary,
                )),
        );
    }
    table
}

/// How much of the window's cost is provider-reported, table-priced, or
/// unpriced — the reader's confidence in the headline number.
fn usage_quality_panel(history: &UsageHistory, theme: &Theme) -> Div {
    let row = |label: String, value: String| {
        div()
            .py(px(8.0))
            .border_b_1()
            .border_color(theme.border)
            .flex()
            .items_center()
            .gap(px(12.0))
            .text_size(px(11.5))
            .child(div().flex_1().text_color(theme.text_secondary).child(label))
            .child(
                div()
                    .text_color(theme.text)
                    .child(SharedString::from(value)),
            )
    };
    div()
        .w(px(240.0))
        .flex_none()
        .flex()
        .flex_col()
        .gap(px(10.0))
        .child(
            div()
                .text_size(px(12.5))
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme.text)
                .child(tr!("usage.cost_quality")),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .child(row(
                    tr!("usage.provider_reported"),
                    format_percent(history.quality.provider_reported_share),
                ))
                .child(row(
                    tr!("usage.model_priced"),
                    format_percent(history.quality.model_priced_share),
                ))
                .child(row(
                    tr!("usage.unpriced"),
                    format_percent(history.quality.unpriced_share),
                ))
                .child(row(
                    tr!("usage.cache_savings"),
                    format_usd(history.quality.cache_savings_usd),
                )),
        )
}

/* ------------------------------------------------------------------------- */
/* Loading skeleton                                                          */
/* ------------------------------------------------------------------------- */

/// Placeholder for the page while the first transcript scan is in flight,
/// shaped like the view it will become and pulsing gently. `with_animation`
/// honors the system's reduce-motion setting on its own.
fn usage_skeleton(view: UsageViewMode, theme: &Theme) -> AnyElement {
    let bar = |width: f32, height: f32| {
        div()
            .h(px(height))
            .w(px(width))
            .flex_none()
            .rounded(px(height / 2.0))
            .bg(theme.overlay_strong)
    };
    let track = || {
        div()
            .h(px(4.0))
            .w_full()
            .flex_none()
            .rounded_full()
            .bg(theme.overlay_strong)
    };

    let body = match view {
        UsageViewMode::Daily => {
            let provider_group = || {
                div()
                    .flex()
                    .flex_col()
                    .gap(px(7.0))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .child(bar(110.0, 12.0))
                            .child(div().flex_1())
                            .child(bar(56.0, 12.0)),
                    )
                    .child(track())
                    .child(bar(150.0, 8.0))
            };
            div()
                .mt(px(20.0))
                .flex()
                .items_start()
                .gap(px(28.0))
                .child(
                    div()
                        .w(px(300.0))
                        .flex_none()
                        .flex()
                        .flex_col()
                        .gap(px(18.0))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(6.0))
                                .child(bar(96.0, 9.0))
                                .child(bar(150.0, 26.0))
                                .child(bar(180.0, 9.0)),
                        )
                        .child(provider_group())
                        .child(provider_group()),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w(px(320.0))
                        .flex()
                        .flex_col()
                        .gap(px(10.0))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .child(bar(90.0, 12.0))
                                .child(div().flex_1())
                                .child(bar(150.0, 10.0)),
                        )
                        .child(
                            div()
                                .h(px(CHART_HEIGHT))
                                .w_full()
                                .rounded(px(8.0))
                                .bg(theme.overlay),
                        )
                        .child(
                            div()
                                .flex()
                                .justify_between()
                                .child(bar(40.0, 8.0))
                                .child(bar(40.0, 8.0))
                                .child(bar(40.0, 8.0)),
                        ),
                )
                .into_any_element()
        }
        UsageViewMode::Monthly | UsageViewMode::Projects => {
            let row = || {
                div()
                    .py(px(13.0))
                    .flex()
                    .flex_col()
                    .gap(px(7.0))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .child(bar(110.0, 12.0))
                            .child(div().flex_1())
                            .child(bar(64.0, 12.0)),
                    )
                    .child(bar(210.0, 8.0))
                    .child(track())
            };
            let mut card = div()
                .mt(px(20.0))
                .px(px(20.0))
                .rounded(px(13.0))
                .bg(theme.raised)
                .flex()
                .flex_col()
                .child(
                    div()
                        .py(px(13.0))
                        .flex()
                        .items_center()
                        .gap(px(16.0))
                        .child(
                            div()
                                .flex_1()
                                .flex()
                                .flex_col()
                                .gap(px(5.0))
                                .child(bar(90.0, 12.0))
                                .child(bar(170.0, 8.0)),
                        )
                        .when(view == UsageViewMode::Projects, |element| {
                            element.child(
                                div()
                                    .h(px(26.0))
                                    .w(px(240.0))
                                    .flex_none()
                                    .rounded(px(7.0))
                                    .bg(theme.overlay_strong),
                            )
                        }),
                );
            for _ in 0..4 {
                card = card.child(row());
            }
            card.into_any_element()
        }
    };

    motion::pulse(Duration::from_millis(1400), move |phase| {
        div()
            .child(body)
            .opacity(pulsating_between(0.45, 0.9)(phase))
            .into_any_element()
    })
    .every(2)
    .into_any_element()
}

/* ------------------------------------------------------------------------- */
/* Monthly statement and shared list vocabulary                              */
/* ------------------------------------------------------------------------- */

/// Whether the lists rank by cost, falling back to tokens when nothing in
/// the window could be priced so the bars still mean something.
fn rank_by_cost(history: &UsageHistory) -> bool {
    history.cost_usd > 0.0
}

fn usage_provider_colors(theme: &Theme) -> [Hsla; 2] {
    [
        provider_color(theme, ProviderKind::Claude),
        provider_color(theme, ProviderKind::Codex),
    ]
}

/// The period total in the ranking unit.
fn usage_headline_value(history: &UsageHistory, by_cost: bool) -> String {
    if by_cost {
        format_usd(history.cost_usd)
    } else {
        format_tokens_compact(history.total_tokens as f64)
    }
}

fn usage_value_label(cost_usd: f64, total_tokens: u64, by_cost: bool) -> String {
    if by_cost {
        format_usd(cost_usd)
    } else {
        format_tokens_compact(total_tokens as f64)
    }
}

/// The card's lead-in row: what the list covers on the left, the period
/// total on the right. Carries the card's horizontal padding itself so its
/// hairline spans the full card width.
fn usage_list_header(theme: &Theme, title: String, caption: String, total: String) -> Div {
    div()
        .px(px(20.0))
        .py(px(13.0))
        .border_b_1()
        .border_color(theme.border)
        .flex()
        .items_center()
        .gap(px(16.0))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .child(
                    div()
                        .text_size(px(13.5))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.text)
                        .child(title),
                )
                .child(
                    div()
                        .mt(px(3.0))
                        .truncate()
                        .text_size(px(10.5))
                        .text_color(theme.text_tertiary)
                        .child(SharedString::from(caption)),
                ),
        )
        .child(
            div()
                .flex_none()
                .text_size(px(15.0))
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme.text)
                .child(SharedString::from(total)),
        )
}

fn usage_list_empty_row(theme: &Theme, message: String) -> Div {
    div()
        .py(px(24.0))
        .flex()
        .justify_center()
        .text_size(px(11.5))
        .text_color(theme.text_tertiary)
        .child(message)
}

/// A magnitude bar over a faint full-width track: `length` is the row's
/// share of the list's peak, and the bar splits into provider segments so
/// one glance carries both size and mix.
fn usage_split_bar(
    theme: &Theme,
    colors: [Hsla; 2],
    length: f32,
    by_provider: &[ProviderDay; 2],
    by_cost: bool,
) -> Div {
    let values = [
        usage_provider_value(&by_provider[0], by_cost),
        usage_provider_value(&by_provider[1], by_cost),
    ];
    let sum = values[0] + values[1];
    let length = if length > 0.0 {
        length.clamp(0.02, 1.0)
    } else {
        0.0
    };
    let mut bar = div()
        .h_full()
        .w(relative(length))
        .rounded_full()
        .overflow_hidden()
        .flex();
    if sum > 0.0 {
        for (index, value) in values.into_iter().enumerate() {
            if value > 0.0 {
                bar = bar.child(
                    div()
                        .h_full()
                        .w(relative((value / sum) as f32))
                        .bg(colors[index]),
                );
            }
        }
    }
    div()
        .h(px(4.0))
        .w_full()
        .rounded_full()
        .bg(theme.overlay)
        .child(bar)
}

fn usage_provider_value(entry: &ProviderDay, by_cost: bool) -> f64 {
    if by_cost {
        entry.cost_usd
    } else {
        entry.total_tokens as f64
    }
}

/// Per-provider amounts with their marks, skipping providers absent from
/// the row.
fn usage_provider_values(theme: &Theme, by_provider: &[ProviderDay; 2], by_cost: bool) -> Div {
    let mut row = div().flex().items_center().gap(px(14.0));
    for provider in UsageProvider::ALL {
        let entry = by_provider[provider.index()];
        if entry.total_tokens == 0 && entry.cost_usd <= 0.0 {
            continue;
        }
        let kind = provider_kind(provider);
        row = row.child(
            div()
                .flex()
                .items_center()
                .gap(px(5.0))
                .child(icon(provider_icon(kind), 11.0, provider_color(theme, kind)))
                .child(
                    div()
                        .text_size(px(10.5))
                        .text_color(theme.text_secondary)
                        .child(SharedString::from(usage_value_label(
                            entry.cost_usd,
                            entry.total_tokens,
                            by_cost,
                        ))),
                ),
        );
    }
    row
}

/// "claude-fable-5 · gpt-5.3-codex · +2" — the row's heaviest models.
fn usage_top_models_label(top_models: &[(String, f64)]) -> Option<String> {
    if top_models.is_empty() {
        return None;
    }
    let mut label = top_models
        .iter()
        .take(2)
        .map(|(name, _)| name.as_str())
        .collect::<Vec<_>>()
        .join(" · ");
    if top_models.len() > 2 {
        label.push_str(&format!(" · +{}", top_models.len() - 2));
    }
    Some(label)
}

/// Lazily built contents of a row's model dropdown. A single custom menu row
/// owns the scrolling list so projects with many models do not grow the card
/// past the window.
fn usage_models_menu_items(top_models: Rc<Vec<(String, f64)>>, total_cost: f64) -> Vec<MenuItem> {
    let count = top_models.len() as u64;
    let header = if total_cost > 0.0 {
        tr!("usage.models_by_spend", models = count_noun(count, "model"))
    } else {
        count_noun(count, "model")
    };
    vec![
        MenuItem::Header(SharedString::from(header)),
        MenuItem::custom(move |_, cx| {
            let theme = Theme::current(cx);
            let mut rows = div()
                .id("usage-models-scroll")
                .w(px(288.0))
                .max_h(px(320.0))
                .overflow_y_scroll()
                .flex()
                .flex_col();
            for (index, (name, cost_usd)) in top_models.iter().enumerate() {
                rows = rows.child(
                    div()
                        .min_h(px(34.0))
                        .py(px(6.0))
                        .when(index > 0, |element| {
                            element.border_t_1().border_color(theme.border)
                        })
                        .flex()
                        .items_center()
                        .gap(px(14.0))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_size(px(11.0))
                                .text_color(theme.text_secondary)
                                .child(SharedString::from(name.clone())),
                        )
                        .when(total_cost > 0.0, |element| {
                            element.child(
                                div()
                                    .flex_none()
                                    .flex()
                                    .flex_col()
                                    .items_end()
                                    .child(
                                        div()
                                            .text_size(px(10.5))
                                            .text_color(theme.text)
                                            .child(SharedString::from(format_usd(*cost_usd))),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(9.0))
                                            .text_color(theme.text_ghost)
                                            .child(SharedString::from(format_percent(
                                                *cost_usd / total_cost,
                                            ))),
                                    ),
                            )
                        }),
                );
            }
            rows.into_any_element()
        }),
    ]
}

/// One tiny bottom-aligned column per day of the month, stacked by provider
/// and scaled against the whole period's busiest day, so quiet and heavy
/// months are honestly comparable at a glance. Decorative texture — every
/// number it hints at is printed in the row.
fn usage_month_strip(
    history: &UsageHistory,
    first_day: NaiveDate,
    peak: f64,
    by_cost: bool,
    colors: [Hsla; 2],
) -> impl IntoElement {
    let day_count = usage_history::days_in_month(first_day);
    let values: Vec<[f64; 2]> = (0..day_count)
        .map(|offset| {
            let day = first_day + chrono::Days::new(u64::from(offset));
            history
                .day(day)
                .map(|slice| {
                    [
                        usage_provider_value(&slice.by_provider[0], by_cost),
                        usage_provider_value(&slice.by_provider[1], by_cost),
                    ]
                })
                .unwrap_or([0.0, 0.0])
        })
        .collect();
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            if peak <= 0.0 {
                return;
            }
            let width = f32::from(bounds.size.width);
            let height = f32::from(bounds.size.height);
            let count = values.len().max(1) as f32;
            let gap = 1.0;
            let bar_width = ((width - gap * (count - 1.0)) / count).max(1.0);
            for (index, bands) in values.iter().enumerate() {
                let x = bounds.origin.x + px(index as f32 * (bar_width + gap));
                let mut top = bounds.origin.y + bounds.size.height;
                for (band, color) in bands.iter().zip(colors) {
                    if *band <= 0.0 {
                        continue;
                    }
                    let band_height = ((*band / peak) as f32 * height).max(1.5);
                    top = top - px(band_height);
                    if top < bounds.origin.y {
                        top = bounds.origin.y;
                    }
                    window.paint_quad(fill(
                        gpui::Bounds::new(
                            point(x, top),
                            gpui::size(px(bar_width), px(band_height)),
                        ),
                        color,
                    ));
                }
            }
        },
    )
    .w(px(168.0))
    .h(px(20.0))
}

/// The statement: one row per calendar month, newest first, from the current
/// month back to the earliest with activity — gap months stay as dim rows so
/// the timeline reads honestly. The card pins its header and scrolls the
/// rows internally, matching the projects view.
fn usage_month_list(
    waku: &Waku,
    history: &UsageHistory,
    theme: &Theme,
    scroll: &ScrollHandle,
    scrollbar_state: &Rc<ScrollbarState>,
    cx: &mut Context<Waku>,
) -> Div {
    let by_cost = rank_by_cost(history);
    let colors = usage_provider_colors(theme);
    let month_value = |month: &MonthSlice| {
        if by_cost {
            month.cost_usd
        } else {
            month.total_tokens as f64
        }
    };
    let peak = history
        .months
        .iter()
        .map(month_value)
        .fold(0.0_f64, f64::max);
    let day_peak = history
        .daily
        .iter()
        .map(|day| {
            if by_cost {
                day.cost_usd
            } else {
                day.total_tokens as f64
            }
        })
        .fold(0.0_f64, f64::max);
    let current_month = usage_history::first_of_month(history.until_day);

    let mut rows = div().px(px(20.0)).flex().flex_col();
    if let Some(earliest) = history.months.first().map(|month| month.first_day) {
        let months = usage_history::enumerate_months(earliest, history.until_day);
        let count = months.len();
        for (index, first_day) in months.iter().rev().enumerate() {
            let last = index + 1 == count;
            rows = rows.child(match history.month(*first_day) {
                Some(month) => {
                    let models_control = waku.usage_models_control(
                        format!("usage-month-models-{first_day}"),
                        &month.top_models,
                        month.cost_usd,
                        theme,
                        cx,
                    );
                    usage_month_row(
                        history,
                        month,
                        theme,
                        colors,
                        by_cost,
                        peak,
                        day_peak,
                        *first_day == current_month,
                        last,
                        models_control,
                    )
                }
                None => usage_empty_month_row(theme, *first_day, last),
            });
        }
    } else {
        rows = rows.child(usage_list_empty_row(
            theme,
            tr!("usage.no_activity_12_months"),
        ));
    }

    div()
        .mt(px(20.0))
        .flex_1()
        .min_h_0()
        .pb(px(8.0))
        .rounded(px(13.0))
        .bg(theme.raised)
        .flex()
        .flex_col()
        .child(
            usage_list_header(
                theme,
                tr!("usage.last_12_months"),
                tr!(
                    "usage.tokens_and_sessions",
                    tokens = format_tokens_compact(history.total_tokens as f64),
                    sessions = count_noun(history.sessions, "session")
                ),
                usage_headline_value(history, by_cost),
            )
            .flex_none(),
        )
        .child(
            // Full-bleed relative wrapper so the overlay scrollbar pins to
            // the card's edge; content padding lives on the rows column.
            div()
                .flex_1()
                .min_h_0()
                .relative()
                .child(
                    div()
                        .id("usage-months-scroll")
                        .size_full()
                        .overflow_y_scroll()
                        .track_scroll(scroll)
                        .child(rows),
                )
                .child(scrollbar::vertical(scroll, scrollbar_state)),
        )
}

#[allow(clippy::too_many_arguments)]
fn usage_month_row(
    history: &UsageHistory,
    month: &MonthSlice,
    theme: &Theme,
    colors: [Hsla; 2],
    by_cost: bool,
    peak: f64,
    day_peak: f64,
    current: bool,
    last: bool,
    models_control: Option<AnyElement>,
) -> Div {
    let value = if by_cost {
        month.cost_usd
    } else {
        month.total_tokens as f64
    };
    div()
        .py(px(13.0))
        .when(!last, |element| {
            element.border_b_1().border_color(theme.border)
        })
        .flex()
        .flex_col()
        .child(
            div()
                .flex()
                .items_baseline()
                .gap(px(8.0))
                .child(
                    div()
                        .text_size(px(13.0))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.text)
                        .child(SharedString::from(format_month(month.first_day))),
                )
                .when(current, |element| {
                    element.child(
                        div()
                            .text_size(px(9.5))
                            .text_color(theme.text_ghost)
                            .child(tr!("usage.so_far")),
                    )
                })
                .child(div().flex_1())
                .child(
                    div()
                        .flex_none()
                        .text_size(px(14.0))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.text)
                        .child(SharedString::from(usage_value_label(
                            month.cost_usd,
                            month.total_tokens,
                            by_cost,
                        ))),
                ),
        )
        .child(
            div()
                .mt(px(2.0))
                .flex()
                .items_end()
                .gap(px(12.0))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(px(10.5))
                        .text_color(theme.text_tertiary)
                        .child(SharedString::from(tr!(
                            "usage.month_caption",
                            tokens = format_tokens_compact(month.total_tokens as f64),
                            sessions = count_noun(month.sessions, "session"),
                            days = count_noun(u64::from(month.active_days), "active day")
                        ))),
                )
                .child(usage_month_strip(
                    history,
                    month.first_day,
                    day_peak,
                    by_cost,
                    colors,
                )),
        )
        .child(div().mt(px(9.0)).child(usage_split_bar(
            theme,
            colors,
            if peak <= 0.0 {
                0.0
            } else {
                (value / peak) as f32
            },
            &month.by_provider,
            by_cost,
        )))
        .child(
            div()
                .mt(px(7.0))
                .flex()
                .items_center()
                .gap(px(12.0))
                .child(usage_provider_values(theme, &month.by_provider, by_cost))
                .child(div().flex_1())
                .when_some(models_control, |element, control| element.child(control)),
        )
}

/// A month inside the covered span with nothing in it. Kept, dimly, so the
/// statement's timeline has no silent holes.
fn usage_empty_month_row(theme: &Theme, first_day: NaiveDate, last: bool) -> Div {
    div()
        .py(px(11.0))
        .when(!last, |element| {
            element.border_b_1().border_color(theme.border)
        })
        .flex()
        .items_baseline()
        .gap(px(8.0))
        .child(
            div()
                .text_size(px(12.5))
                .text_color(theme.text_tertiary)
                .child(SharedString::from(format_month(first_day))),
        )
        .child(div().flex_1())
        .child(
            div()
                .text_size(px(10.0))
                .text_color(theme.text_ghost)
                .child(tr!("usage.no_activity")),
        )
}

/* ------------------------------------------------------------------------- */
/* Chart math                                                                */
/* ------------------------------------------------------------------------- */

/// One cubic segment of a smoothed series boundary, in window pixels.
struct CurveSegment {
    from: (f32, f32),
    c1: (f32, f32),
    c2: (f32, f32),
    to: (f32, f32),
}

/// Monotone cubic tangents (Fritsch–Carlson). Plain cubic smoothing
/// overshoots on spiky daily data and would dip the area below zero between
/// points, which reads as negative spend; this variant is shape-preserving,
/// so a smoothed series never leaves the range of its samples.
fn monotone_tangents(points: &[(f32, f32)]) -> Vec<f32> {
    let count = points.len();
    if count < 2 {
        return vec![0.0];
    }
    let mut slopes = Vec::with_capacity(count - 1);
    for index in 0..count - 1 {
        let dx = points[index + 1].0 - points[index].0;
        let dy = points[index + 1].1 - points[index].1;
        slopes.push(if dx == 0.0 { 0.0 } else { dy / dx });
    }

    let mut tangents = vec![0.0; count];
    tangents[0] = slopes[0];
    tangents[count - 1] = slopes[count - 2];
    for index in 1..count - 1 {
        let previous = slopes[index - 1];
        let next = slopes[index];
        tangents[index] = if previous * next <= 0.0 {
            0.0
        } else {
            (previous + next) / 2.0
        };
    }

    for index in 0..count - 1 {
        let slope = slopes[index];
        if slope == 0.0 {
            tangents[index] = 0.0;
            tangents[index + 1] = 0.0;
            continue;
        }
        let a = tangents[index] / slope;
        let b = tangents[index + 1] / slope;
        let magnitude = a * a + b * b;
        if magnitude > 9.0 {
            let scale = 3.0 / magnitude.sqrt();
            tangents[index] = scale * a * slope;
            tangents[index + 1] = scale * b * slope;
        }
    }
    tangents
}

/// Smoothed polyline through `points`, as explicit cubic control points.
fn smooth_curve(points: &[(f32, f32)]) -> Vec<CurveSegment> {
    if points.len() < 2 {
        return Vec::new();
    }
    let tangents = monotone_tangents(points);
    let mut segments = Vec::with_capacity(points.len() - 1);
    for index in 0..points.len() - 1 {
        let from = points[index];
        let to = points[index + 1];
        let dx = to.0 - from.0;
        segments.push(CurveSegment {
            from,
            c1: (from.0 + dx / 3.0, from.1 + tangents[index] * dx / 3.0),
            c2: (to.0 - dx / 3.0, to.1 - tangents[index + 1] * dx / 3.0),
            to,
        });
    }
    segments
}

/// A scale whose maximum is a readable 1/2/5 × 10ⁿ step at or above the
/// peak. Rounding the maximum *up* is the point: stopping at the last step
/// below the peak leaves the tallest day drawn past the top of the plot,
/// where it is clipped.
fn nice_scale(peak: f64, count: usize) -> (f64, Vec<f64>) {
    if peak <= 0.0 {
        return (0.0, vec![0.0]);
    }
    let raw_step = peak / count as f64;
    let magnitude = 10.0_f64.powf(raw_step.log10().floor());
    let normalized = raw_step / magnitude;
    let step = if normalized > 5.0 {
        10.0
    } else if normalized > 2.0 {
        5.0
    } else if normalized > 1.0 {
        2.0
    } else {
        1.0
    } * magnitude;
    let max = (peak / step).ceil() * step;
    let mut ticks = Vec::new();
    let mut value = 0.0;
    while value <= max + step * 1e-6 {
        ticks.push(value);
        value += step;
    }
    (max, ticks)
}

/* ------------------------------------------------------------------------- */
/* Formatting                                                                */
/* ------------------------------------------------------------------------- */

fn group_thousands(number: &str) -> String {
    let (integer, fraction) = match number.split_once('.') {
        Some((integer, fraction)) => (integer, Some(fraction)),
        None => (number, None),
    };
    let grouped = integer
        .as_bytes()
        .rchunks(3)
        .rev()
        .map(|chunk| std::str::from_utf8(chunk).unwrap_or_default())
        .collect::<Vec<_>>()
        .join(",");
    match fraction {
        Some(fraction) => format!("{grouped}.{fraction}"),
        None => grouped,
    }
}

fn format_usd(value: f64) -> String {
    format!("${}", group_thousands(&format!("{:.2}", value.max(0.0))))
}

fn format_count(value: u64) -> String {
    group_thousands(&value.to_string())
}

/// Compacts a token count to three significant figures with a unit suffix, so
/// columns of numbers line up at a glance (`19.9B`, `76.7M`, `804K`).
fn format_tokens_compact(value: f64) -> String {
    let abs = value.abs();
    let (scaled, suffix) = if abs >= 1e12 {
        (value / 1e12, "T")
    } else if abs >= 1e9 {
        (value / 1e9, "B")
    } else if abs >= 1e6 {
        (value / 1e6, "M")
    } else if abs >= 1e3 {
        (value / 1e3, "K")
    } else {
        return format_count(value.round().max(0.0) as u64);
    };
    let digits = if scaled.abs() >= 100.0 {
        0
    } else if scaled.abs() >= 10.0 {
        1
    } else {
        2
    };
    let mut text = format!("{scaled:.digits$}");
    // Trim an all-zero fraction ("1.00" → "1") but keep "1.50".
    if let Some(dot) = text.find('.')
        && text[dot + 1..].bytes().all(|byte| byte == b'0')
    {
        text.truncate(dot);
    }
    format!("{text}{suffix}")
}

fn format_percent(share: f64) -> String {
    format!("{:.1}%", share * 100.0)
}

/// Keep the full project path, abbreviating only the user's home directory.
fn usage_project_path(path: &Path, home: Option<&Path>) -> String {
    match home.and_then(|home| path.strip_prefix(home).ok()) {
        Some(relative) if relative.as_os_str().is_empty() => "~".to_owned(),
        Some(relative) => format!("~/{}", relative.display()),
        None => path.display().to_string(),
    }
}

/// `2026-08-07` → `Aug 7`.
fn format_day_short(day: NaiveDate) -> String {
    if crate::i18n::uses_east_asian_date_format() {
        format!("{}月{}日", day.month(), day.day())
    } else {
        day.format("%b %-d").to_string()
    }
}

/// `2026-08-01` → `August 2026`.
fn format_month(first_day: NaiveDate) -> String {
    if crate::i18n::uses_east_asian_date_format() {
        format!("{}年{}月", first_day.year(), first_day.month())
    } else {
        first_day.format("%B %Y").to_string()
    }
}

/// `2025-09-01` → `Sep 2025`.
fn format_month_short(day: NaiveDate) -> String {
    if crate::i18n::uses_east_asian_date_format() {
        format!("{}年{}月", day.year(), day.month())
    } else {
        day.format("%b %Y").to_string()
    }
}

/// Menu label for a selectable window.
fn window_choice_label(window: UsageWindow) -> String {
    match window {
        UsageWindow::TrailingDays(7) => tr!("usage.last_7_days"),
        UsageWindow::TrailingDays(30) => tr!("usage.last_30_days"),
        UsageWindow::TrailingDays(90) => tr!("usage.last_90_days"),
        UsageWindow::ThisMonth => tr!("usage.this_month"),
        UsageWindow::LastMonth => tr!("usage.last_month"),
        // Not offered by the selector; only the statement view uses these.
        UsageWindow::TrailingDays(_) | UsageWindow::Months(_) => tr!("usage.custom"),
    }
}

/// `1 session`, `214 sessions` — grouped count with a pluralized noun.
fn count_noun(count: u64, noun: &str) -> String {
    let (singular, plural) = match noun {
        "project" => ("usage.project_one", "usage.project_many"),
        "session" => ("usage.session_one", "usage.session_many"),
        "model" => ("usage.model_one", "usage.model_many"),
        "active day" => ("usage.active_day_one", "usage.active_day_many"),
        _ => return format!("{} {noun}", format_count(count)),
    };
    tr!(
        if count == 1 { singular } else { plural },
        count = format_count(count)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_counts_compact_to_three_significant_figures() {
        assert_eq!(format_tokens_compact(804_000.0), "804K");
        assert_eq!(format_tokens_compact(76_700_000.0), "76.7M");
        assert_eq!(format_tokens_compact(19_900_000_000.0), "19.9B");
        assert_eq!(format_tokens_compact(950.0), "950");
        assert_eq!(format_tokens_compact(1_000.0), "1K");
        assert_eq!(format_tokens_compact(1_500.0), "1.50K");
    }

    #[test]
    fn model_summary_names_the_hidden_count() {
        let models = vec![
            ("gpt-5.6-sol".to_owned(), 30.0),
            ("claude-fable-5".to_owned(), 20.0),
            ("gpt-5.6-luna".to_owned(), 10.0),
        ];

        assert_eq!(usage_top_models_label(&[]), None);
        assert_eq!(
            usage_top_models_label(&models[..2]).as_deref(),
            Some("gpt-5.6-sol · claude-fable-5")
        );
        assert_eq!(
            usage_top_models_label(&models).as_deref(),
            Some("gpt-5.6-sol · claude-fable-5 · +1")
        );
    }

    #[test]
    fn currency_groups_thousands() {
        assert_eq!(format_usd(0.0), "$0.00");
        assert_eq!(format_usd(1_234.5), "$1,234.50");
        assert_eq!(format_usd(1_234_567.891), "$1,234,567.89");
    }

    #[test]
    fn project_paths_only_shorten_the_home_prefix() {
        let home = Path::new("/Users/developer");

        assert_eq!(
            usage_project_path(Path::new("/Users/developer/dev/waku"), Some(home)),
            "~/dev/waku"
        );
        assert_eq!(usage_project_path(home, Some(home)), "~");
        assert_eq!(
            usage_project_path(Path::new("/Users/developer-2/waku"), Some(home)),
            "/Users/developer-2/waku"
        );
        assert_eq!(
            usage_project_path(Path::new("/Volumes/work/waku"), Some(home)),
            "/Volumes/work/waku"
        );
    }

    #[test]
    fn nice_scales_round_up_to_readable_steps() {
        let (max, ticks) = nice_scale(97.0, 4);
        assert_eq!(max, 100.0);
        assert_eq!(ticks, vec![0.0, 50.0, 100.0]);
        let (max, _) = nice_scale(0.37, 4);
        assert!(max >= 0.37);
        let (max, ticks) = nice_scale(0.0, 4);
        assert_eq!(max, 0.0);
        assert_eq!(ticks, vec![0.0]);
    }

    #[test]
    fn monotone_smoothing_never_overshoots_flat_runs() {
        // A spike between two flat runs: the flat segments must stay flat
        // (zero tangents), which is what keeps the area fill from dipping
        // below zero.
        let points = [
            (0.0, 100.0),
            (10.0, 100.0),
            (20.0, 0.0),
            (30.0, 100.0),
            (40.0, 100.0),
        ];
        let tangents = monotone_tangents(&points);
        assert_eq!(tangents[0], 0.0);
        assert_eq!(tangents[1], 0.0);
        assert_eq!(tangents[3], 0.0);
        assert_eq!(tangents[4], 0.0);
        let segments = smooth_curve(&points);
        assert_eq!(segments.len(), 4);
        for segment in &segments {
            for y in [segment.c1.1, segment.c2.1] {
                assert!(
                    (-0.001..=100.001).contains(&y),
                    "control point left range: {y}"
                );
            }
        }
    }
}

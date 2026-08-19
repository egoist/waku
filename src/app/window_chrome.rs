#[cfg(target_os = "linux")]
use gpui::WindowButtonLayout;
#[cfg(target_os = "windows")]
use gpui::WindowControlArea;
#[cfg(target_os = "linux")]
use gpui::svg;
use gpui::{
    AnyElement, BoxShadow, Context, Decorations, Div, Hsla, IntoElement, MouseButton, ResizeEdge,
    Tiling, Window, div, prelude::*, px, transparent_black,
};
#[cfg(any(target_os = "linux", target_os = "windows"))]
use gpui::{KeyDownEvent, WindowButton};

use super::Waku;
#[cfg(target_os = "linux")]
use crate::theme::RADIUS_LG;
use crate::theme::{RADIUS_DF, Theme};
#[cfg(any(target_os = "linux", target_os = "windows"))]
use crate::ui::tooltip::Tooltip;

const CLIENT_FRAME_INSET: f32 = 10.0;

#[derive(Clone, Copy)]
pub(super) enum WindowControlSide {
    Left,
    Right,
}

impl Waku {
    /// Draw the frame a Wayland compositor delegates back to the client.
    /// Server-decorated windows pass through untouched, so X11 and Wayland
    /// compositors that provide native chrome keep doing so.
    pub(super) fn render_window_frame(
        &self,
        content: AnyElement,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if window.is_fullscreen() {
            window.set_client_inset(px(0.0));
            return content;
        }
        let Decorations::Client { tiling } = window.window_decorations() else {
            window.set_client_inset(px(0.0));
            return content;
        };

        let inset = px(CLIENT_FRAME_INSET);
        let rounding = px(RADIUS_DF);
        let border = px(1.0);
        let theme = Theme::current(cx);
        window.set_client_inset(inset);

        let frame = div()
            .relative()
            .size_full()
            .overflow_hidden()
            .when(!(tiling.top || tiling.left), |frame| {
                frame.rounded_tl(rounding)
            })
            .when(!(tiling.top || tiling.right), |frame| {
                frame.rounded_tr(rounding)
            })
            .when(!(tiling.bottom || tiling.left), |frame| {
                frame.rounded_bl(rounding)
            })
            .when(!(tiling.bottom || tiling.right), |frame| {
                frame.rounded_br(rounding)
            })
            .when(!tiling.top, |frame| frame.border_t(border))
            .when(!tiling.bottom, |frame| frame.border_b(border))
            .when(!tiling.left, |frame| frame.border_l(border))
            .when(!tiling.right, |frame| frame.border_r(border))
            .border_color(theme.border_strong)
            .when(!tiling.is_tiled(), |frame| {
                frame.shadow(vec![
                    BoxShadow::new(
                        px(0.0),
                        px(0.0),
                        Hsla {
                            h: 0.0,
                            s: 0.0,
                            l: 0.0,
                            a: if theme.is_dark { 0.5 } else { 0.25 },
                        },
                    )
                    .blur_radius(inset / 2.0),
                ])
            })
            .child(content);

        div()
            .id("client-window-backdrop")
            .relative()
            .size_full()
            .bg(transparent_black())
            .when(!tiling.top, |backdrop| backdrop.pt(inset))
            .when(!tiling.bottom, |backdrop| backdrop.pb(inset))
            .when(!tiling.left, |backdrop| backdrop.pl(inset))
            .when(!tiling.right, |backdrop| backdrop.pr(inset))
            .child(frame)
            .children(
                window
                    .is_resizable()
                    .then(|| client_resize_handles(tiling, inset))
                    .into_iter()
                    .flatten(),
            )
            .into_any_element()
    }

    /// Render the window controls Waku owns: the desktop's configured button
    /// order when GPUI had to fall back from server-side to client-side
    /// decorations, and the platform order on Windows.
    pub(super) fn render_client_window_controls(
        &self,
        side: WindowControlSide,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if window.is_fullscreen() {
            return None;
        }
        #[cfg(target_os = "windows")]
        let buttons = {
            // Windows keeps all three on the right, in this order, and has no
            // per-user layout preference to read.
            if !matches!(side, WindowControlSide::Right) {
                return None;
            }
            [
                Some(WindowButton::Minimize),
                Some(WindowButton::Maximize),
                Some(WindowButton::Close),
            ]
        };

        #[cfg(target_os = "linux")]
        let buttons = {
            if !matches!(window.window_decorations(), Decorations::Client { .. }) {
                return None;
            }
            let layout = cx
                .button_layout()
                .unwrap_or_else(WindowButtonLayout::linux_default);
            match side {
                WindowControlSide::Left => layout.left,
                WindowControlSide::Right => layout.right,
            }
        };

        #[cfg(any(target_os = "linux", target_os = "windows"))]
        {
            if buttons.iter().all(Option::is_none) {
                return None;
            }

            let theme = Theme::current(cx);
            let is_maximized = window.is_maximized();
            let supported = window.window_controls();
            let side_id = match side {
                WindowControlSide::Left => "client-window-controls-left",
                WindowControlSide::Right => "client-window-controls-right",
            };
            let control_elements = buttons.into_iter().flatten().map(|button| {
                let enabled = match button {
                    WindowButton::Minimize => supported.minimize && window.is_minimizable(),
                    WindowButton::Maximize => supported.maximize && window.is_resizable(),
                    WindowButton::Close => true,
                };
                client_window_button(button, enabled, is_maximized, theme, cx)
            });

            let controls = div()
                .id(side_id)
                .tab_group()
                .tab_stop(false)
                .h_full()
                .flex_none()
                .flex()
                .items_center();
            #[cfg(target_os = "linux")]
            let controls = controls.px(px(10.0)).gap(px(8.0));
            Some(controls.children(control_elements).into_any_element())
        }

        #[cfg(not(any(target_os = "linux", target_os = "windows")))]
        {
            let _ = (side, window, cx);
            None
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "windows"))]
fn client_window_button(
    button: WindowButton,
    enabled: bool,
    is_maximized: bool,
    theme: Theme,
    cx: &mut Context<Waku>,
) -> AnyElement {
    let (id, label) = match button {
        WindowButton::Minimize => ("client-window-minimize", tr!("window.minimize")),
        WindowButton::Maximize if is_maximized => ("client-window-restore", tr!("window.restore")),
        WindowButton::Maximize => ("client-window-maximize", tr!("window.maximize")),
        WindowButton::Close => ("client-window-close", tr!("menu.close_window")),
    };
    let focus = cx.focus_handle();
    let icon_color = if enabled {
        theme.text_secondary
    } else {
        theme.text_ghost
    };

    let control = div().id(id);
    // Windows hit-tests the caption before it dispatches a mouse event.
    // Claiming the button's area here is also what lets the maximize control
    // show Windows 11's snap layouts on hover.
    #[cfg(target_os = "windows")]
    let control = control.window_control_area(match button {
        WindowButton::Minimize => WindowControlArea::Min,
        WindowButton::Maximize => WindowControlArea::Max,
        WindowButton::Close => WindowControlArea::Close,
    });

    let control = control
        .track_focus(&focus)
        .tab_index(0)
        .tab_stop(true)
        .occlude()
        .group(id)
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .cursor_default()
        .opacity(if enabled { 1.0 } else { 0.45 })
        .focus_visible(|style| style.border_1().border_color(theme.ring))
        .when(enabled, |control| {
            control
                .hover(move |style| {
                    if button == WindowButton::Close {
                        style.bg(theme.danger)
                    } else {
                        style.bg(theme.overlay)
                    }
                })
                .active(move |style| {
                    if button == WindowButton::Close {
                        style.bg(theme.danger.opacity(0.8))
                    } else {
                        style.bg(theme.overlay_strong)
                    }
                })
        });
    #[cfg(target_os = "windows")]
    let control = control.w(px(36.0)).h_full();
    #[cfg(target_os = "linux")]
    let control = control.size(px(26.0)).rounded(px(RADIUS_LG));

    let hovered_icon_color = if button == WindowButton::Close {
        theme.on_inverse
    } else {
        theme.text
    };
    #[cfg(target_os = "windows")]
    let glyph = div()
        .font_family(windows_caption_font())
        .text_size(px(10.0))
        .text_color(icon_color)
        .group_hover(id, move |glyph| glyph.text_color(hovered_icon_color))
        .child(match button {
            WindowButton::Minimize => "\u{e921}",
            WindowButton::Maximize if is_maximized => "\u{e923}",
            WindowButton::Maximize => "\u{e922}",
            WindowButton::Close => "\u{e8bb}",
        });
    #[cfg(target_os = "linux")]
    let glyph = svg()
        .path(match button {
            WindowButton::Minimize => "icons/window-minimize.svg",
            WindowButton::Maximize if is_maximized => "icons/window-restore.svg",
            WindowButton::Maximize => "icons/window-maximize.svg",
            WindowButton::Close => "icons/x.svg",
        })
        .size(px(14.0))
        .flex_none()
        .text_color(icon_color)
        .group_hover(id, move |glyph| glyph.text_color(hovered_icon_color));
    let control = control.tooltip(Tooltip::text(label)).child(glyph);
    #[cfg(target_os = "linux")]
    let control = control
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(move |_, window, cx| {
            cx.stop_propagation();
            if enabled {
                activate_window_button(button, window);
            }
        });

    control
        .on_key_down(move |event: &KeyDownEvent, window, cx| {
            if enabled
                && !event.keystroke.modifiers.modified()
                && matches!(event.keystroke.key.as_str(), "enter" | "space")
            {
                activate_window_button(button, window);
                cx.stop_propagation();
            }
        })
        .into_any_element()
}

#[cfg(target_os = "windows")]
fn windows_caption_font() -> &'static str {
    use windows::Wdk::System::SystemServices::RtlGetVersion;

    let mut version = unsafe { std::mem::zeroed() };
    let status = unsafe { RtlGetVersion(&mut version) };
    if status.is_ok() && version.dwBuildNumber >= 22000 {
        "Segoe Fluent Icons"
    } else {
        "Segoe MDL2 Assets"
    }
}

impl Waku {
    pub(super) fn toggle_fullscreen_action(
        &mut self,
        _: &crate::ToggleFullscreen,
        window: &mut Window,
        _: &mut Context<Self>,
    ) {
        window.toggle_fullscreen();
    }
}

#[cfg(any(target_os = "linux", target_os = "windows"))]
fn activate_window_button(button: WindowButton, window: &mut Window) {
    match button {
        WindowButton::Minimize => window.minimize_window(),
        WindowButton::Maximize => window.zoom_window(),
        WindowButton::Close => crate::platform::hide_window(window),
    }
}

fn client_resize_handles(tiling: Tiling, inset: gpui::Pixels) -> Vec<AnyElement> {
    let mut handles = Vec::with_capacity(8);
    if !tiling.top {
        handles.push(client_resize_handle(ResizeEdge::Top, inset));
    }
    if !tiling.bottom {
        handles.push(client_resize_handle(ResizeEdge::Bottom, inset));
    }
    if !tiling.left {
        handles.push(client_resize_handle(ResizeEdge::Left, inset));
    }
    if !tiling.right {
        handles.push(client_resize_handle(ResizeEdge::Right, inset));
    }
    if !tiling.top && !tiling.left {
        handles.push(client_resize_handle(ResizeEdge::TopLeft, inset));
    }
    if !tiling.top && !tiling.right {
        handles.push(client_resize_handle(ResizeEdge::TopRight, inset));
    }
    if !tiling.bottom && !tiling.left {
        handles.push(client_resize_handle(ResizeEdge::BottomLeft, inset));
    }
    if !tiling.bottom && !tiling.right {
        handles.push(client_resize_handle(ResizeEdge::BottomRight, inset));
    }
    handles
}

fn client_resize_handle(edge: ResizeEdge, inset: gpui::Pixels) -> AnyElement {
    let (id, handle): (&'static str, Div) = match edge {
        ResizeEdge::Top => (
            "client-resize-top",
            div()
                .absolute()
                .top_0()
                .left(inset)
                .right(inset)
                .h(inset)
                .cursor_n_resize(),
        ),
        ResizeEdge::Bottom => (
            "client-resize-bottom",
            div()
                .absolute()
                .bottom_0()
                .left(inset)
                .right(inset)
                .h(inset)
                .cursor_s_resize(),
        ),
        ResizeEdge::Left => (
            "client-resize-left",
            div()
                .absolute()
                .top(inset)
                .bottom(inset)
                .left_0()
                .w(inset)
                .cursor_w_resize(),
        ),
        ResizeEdge::Right => (
            "client-resize-right",
            div()
                .absolute()
                .top(inset)
                .bottom(inset)
                .right_0()
                .w(inset)
                .cursor_e_resize(),
        ),
        ResizeEdge::TopLeft => (
            "client-resize-top-left",
            div()
                .absolute()
                .top_0()
                .left_0()
                .size(inset)
                .cursor_nwse_resize(),
        ),
        ResizeEdge::TopRight => (
            "client-resize-top-right",
            div()
                .absolute()
                .top_0()
                .right_0()
                .size(inset)
                .cursor_nesw_resize(),
        ),
        ResizeEdge::BottomLeft => (
            "client-resize-bottom-left",
            div()
                .absolute()
                .bottom_0()
                .left_0()
                .size(inset)
                .cursor_nesw_resize(),
        ),
        ResizeEdge::BottomRight => (
            "client-resize-bottom-right",
            div()
                .absolute()
                .bottom_0()
                .right_0()
                .size(inset)
                .cursor_nwse_resize(),
        ),
    };

    handle
        .id(id)
        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
            cx.stop_propagation();
            window.start_window_resize(edge);
        })
        .into_any_element()
}

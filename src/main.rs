mod app;
mod assets;
mod checkpoint;
mod driver;
mod input;
mod model;
mod model_catalog;
mod persistence;
mod platform;
mod theme;
mod ui;

use gpui::{
    App, AppContext, Application, Bounds, KeyBinding, Menu, MenuItem, TitlebarOptions,
    WindowBounds, WindowOptions, actions, point, px, size,
};

use crate::app::Waku;
use crate::input::{
    Backspace, Copy, Cut, Delete, End, Enter, Home, Left, Paste, Right, SelectAll, SelectLeft,
    SelectRight,
};

actions!(
    waku,
    [Quit, NewSession, ToggleSidebar, FocusComposer, CancelTurn]
);

fn main() {
    Application::new()
        .with_assets(crate::assets::Assets)
        .run(|cx: &mut App| {
            gpui_component::init(cx);
            crate::theme::init_component_theme(cx);
            cx.set_menus(vec![
                Menu {
                    name: "Waku".into(),
                    items: vec![
                        MenuItem::action("New Session", NewSession),
                        MenuItem::separator(),
                        MenuItem::action("Quit Waku", Quit),
                    ],
                },
                Menu {
                    name: "View".into(),
                    items: vec![
                        MenuItem::action("Toggle Sidebar", ToggleSidebar),
                        MenuItem::action("Focus Composer", FocusComposer),
                    ],
                },
            ]);

            cx.bind_keys([
                KeyBinding::new("cmd-q", Quit, None),
                KeyBinding::new("cmd-n", NewSession, None),
                KeyBinding::new("cmd-shift-s", ToggleSidebar, None),
                KeyBinding::new("cmd-l", FocusComposer, None),
                KeyBinding::new("escape", CancelTurn, Some("Waku")),
                KeyBinding::new("backspace", Backspace, Some("ComposerInput")),
                KeyBinding::new("delete", Delete, Some("ComposerInput")),
                KeyBinding::new("left", Left, Some("ComposerInput")),
                KeyBinding::new("right", Right, Some("ComposerInput")),
                KeyBinding::new("shift-left", SelectLeft, Some("ComposerInput")),
                KeyBinding::new("shift-right", SelectRight, Some("ComposerInput")),
                KeyBinding::new("cmd-a", SelectAll, Some("ComposerInput")),
                KeyBinding::new("cmd-v", Paste, Some("ComposerInput")),
                KeyBinding::new("cmd-c", Copy, Some("ComposerInput")),
                KeyBinding::new("cmd-x", Cut, Some("ComposerInput")),
                KeyBinding::new("home", Home, Some("ComposerInput")),
                KeyBinding::new("end", End, Some("ComposerInput")),
                KeyBinding::new("enter", Enter, Some("ComposerInput")),
            ]);

            cx.on_action(|_: &Quit, cx| cx.quit());

            let bounds = Bounds::centered(None, size(px(1380.0), px(880.0)), cx);

            let window = cx
                .open_window(
                    WindowOptions {
                        titlebar: Some(TitlebarOptions {
                            title: Some("Waku".into()),
                            appears_transparent: true,
                            traffic_light_position: Some(point(px(16.0), px(18.0))),
                        }),
                        window_bounds: Some(WindowBounds::Windowed(bounds)),
                        window_min_size: Some(size(px(980.0), px(680.0))),
                        ..Default::default()
                    },
                    |window, cx| {
                        let waku = Waku::new(window, cx);
                        let composer_focus = waku.read(cx).composer_focus(cx);
                        window.focus(&composer_focus);
                        cx.new(|cx| gpui_component::Root::new(waku, window, cx))
                    },
                )
                .expect("failed to open Waku window");

            window
                .update(cx, |_, _, cx| {
                    cx.activate(true);
                })
                .ok();
        });
}

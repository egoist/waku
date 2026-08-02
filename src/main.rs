#![recursion_limit = "256"]

mod amp_session;
mod app;
mod assets;
mod checkpoint;
mod claude_session;
mod command_env;
mod computer_use;
mod cursor_session;
mod driver;
mod grok_session;
mod identity;
mod input;
mod model;
mod model_catalog;
mod opencode_session;
mod persistence;
mod platform;
mod terminal;
mod theme;
mod ui;

use gpui::{
    App, AppContext, Application, Bounds, KeyBinding, Menu, MenuItem, TitlebarOptions,
    WindowBackgroundAppearance, WindowBounds, WindowOptions, actions, point, px, size,
};

use crate::app::Waku;
use crate::identity::APP_NAME;
actions!(
    waku,
    [
        Quit,
        CloseWindow,
        NewSession,
        OpenSettings,
        ToggleSidebar,
        ToggleRightPanel,
        NavigateBack,
        NavigateForward,
        FocusComposer,
        SaveFile,
        CancelTurn
    ]
);

trait WakuApplicationExt {
    fn with_main_window_reopen(self) -> Self;
}

impl WakuApplicationExt for Application {
    fn with_main_window_reopen(self) -> Self {
        self.on_reopen(|cx| {
            if let Some(window) = cx.windows().into_iter().next() {
                window
                    .update(cx, |_, window, _| window.activate_window())
                    .ok();
            }
            cx.activate(true);
        });
        self
    }
}

fn main() {
    gpui_platform::application()
        .with_assets(crate::assets::Assets)
        .with_main_window_reopen()
        .run(|cx: &mut App| {
            gpui_component::init(cx);
            crate::assets::register_fonts(cx).expect("failed to register bundled fonts");
            crate::input::init(cx);
            crate::theme::init_component_theme(cx);
            cx.set_reduce_motion(crate::platform::reduce_motion_enabled());
            cx.set_menus(vec![
                Menu {
                    name: APP_NAME.into(),
                    disabled: false,
                    items: vec![
                        MenuItem::action("New Session", NewSession),
                        MenuItem::separator(),
                        MenuItem::action("Settings…", OpenSettings),
                        MenuItem::separator(),
                        MenuItem::action(format!("Quit {APP_NAME}"), Quit),
                    ],
                },
                Menu {
                    name: "View".into(),
                    disabled: false,
                    items: vec![
                        MenuItem::action("Toggle Sidebar", ToggleSidebar),
                        MenuItem::action("Toggle Right Panel", ToggleRightPanel),
                        MenuItem::action("Focus Composer", FocusComposer),
                    ],
                },
            ]);

            cx.bind_keys([
                KeyBinding::new("cmd-q", Quit, None),
                KeyBinding::new("cmd-w", CloseWindow, None),
                KeyBinding::new("cmd-n", NewSession, None),
                KeyBinding::new("cmd-,", OpenSettings, None),
                KeyBinding::new("cmd-b", ToggleSidebar, None),
                KeyBinding::new("cmd-shift-b", ToggleRightPanel, None),
                KeyBinding::new("cmd-[", NavigateBack, Some("Waku")),
                KeyBinding::new("cmd-]", NavigateForward, Some("Waku")),
                KeyBinding::new("cmd-l", FocusComposer, None),
                KeyBinding::new("cmd-s", SaveFile, None),
                KeyBinding::new("escape", CancelTurn, Some("Waku")),
            ]);

            cx.on_action(|_: &Quit, cx| cx.quit());

            let bounds = Bounds::centered(None, size(px(1380.0), px(880.0)), cx);

            let window = cx
                .open_window(
                    WindowOptions {
                        titlebar: Some(TitlebarOptions {
                            title: Some(APP_NAME.into()),
                            appears_transparent: true,
                            traffic_light_position: Some(point(px(16.0), px(17.0))),
                        }),
                        // Waku owns titlebar gestures so controls embedded in the header
                        // never inherit AppKit's implicit drag/double-click behavior.
                        is_movable: false,
                        window_background: WindowBackgroundAppearance::Blurred,
                        window_bounds: Some(WindowBounds::Windowed(bounds)),
                        window_min_size: Some(size(px(980.0), px(680.0))),
                        ..Default::default()
                    },
                    |window, cx| {
                        crate::platform::configure_main_window_close_behavior(window, cx);
                        let waku = Waku::new(window, cx);
                        let composer_focus = waku.read(cx).composer_focus(cx);
                        window.focus(&composer_focus, cx);
                        cx.new(|cx| {
                            gpui_component::Root::new(waku, window, cx)
                                .background(gpui::transparent_black())
                        })
                    },
                )
                .expect("failed to open Waku window");

            window
                .update(cx, |_, window, cx| {
                    crate::platform::configure_sidebar_material(
                        window,
                        crate::theme::Theme::current(cx).is_dark,
                    );
                    cx.activate(true);
                })
                .ok();
        });
}

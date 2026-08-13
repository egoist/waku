#![recursion_limit = "256"]

rust_i18n::i18n!("locales", fallback = "en");

macro_rules! tr {
    ($key:expr) => {
        crate::i18n::translate($key)
    };
    ($key:expr, $($args:tt)*) => {
        rust_i18n::t!($key, $($args)*).into_owned()
    };
}

/// Borrow static translations on hot render paths; interpolation uses `tr!`
/// because formatted messages necessarily allocate.
macro_rules! tr_cow {
    ($key:literal) => {
        rust_i18n::t!($key)
    };
}

mod amp_session;
mod analytics;
mod app;
mod assets;
mod blob_store;
mod browser;
mod checkpoint;
mod claude_session;
mod command_env;
mod composer_complete;
mod computer_use;
mod cursor_session;
mod deepseek_pool;
mod deepseek_session;
mod driver;
mod git_branch;
mod git_commit;
mod grok_session;
mod i18n;
mod identity;
mod input;
mod md;
mod model;
mod model_catalog;
mod opencode_pool;
mod opencode_session;
mod persistence;
mod platform;
mod projectless;
mod query;
mod review_diff;
mod skills;
mod terminal;
mod theme;
mod ui;
mod updater;
mod usage;
mod usage_history;
mod worktree;

use gpui::{
    App, Application, Bounds, KeyBinding, Menu, MenuItem, TitlebarOptions,
    WindowBackgroundAppearance, WindowBounds, WindowOptions, actions, point, px, size,
};

use crate::app::Waku;
use crate::identity::APP_NAME;
actions!(
    waku,
    [
        Quit,
        About,
        CloseWindow,
        NewSession,
        NewProject,
        OpenSettings,
        CheckForUpdates,
        ToggleSidebar,
        ToggleRightPanel,
        ToggleCommandPalette,
        ToggleFpsCounter,
        NavigateBack,
        NavigateForward,
        FocusComposer,
        ToggleModelPicker,
        ToggleUsagePanel,
        SaveFile,
        CancelTurn,
        CopySelection,
        OpenFind,
        OpenFindReplace,
        CloseFind,
        FindNext,
        FindPrevious,
        ToggleFindCaseSensitive,
        ToggleFindWholeWord,
        ToggleFindRegex,
        ReplaceAllMatches,
        BrowserBack,
        BrowserForward,
        BrowserReload,
        BrowserHardReload,
        BrowserStop,
        BrowserDevtools,
        FocusBrowserAddress,
        BrowserAddressCancel,
        WebviewCopy,
        WebviewCut,
        WebviewPaste,
        WebviewSelectAll
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
            crate::assets::register_fonts(cx).expect("failed to register bundled fonts");
            crate::input::init(cx);
            crate::ui::menu::init(cx);
            crate::app::init_composer_autocomplete(cx);
            crate::app::init_settings_keys(cx);
            crate::app::init_command_palette(cx);
            crate::app::init_commit_dialog_keys(cx);
            crate::app::init_image_preview_keys(cx);
            crate::app::init_sidebar_keys(cx);
            crate::app::init_skills_keys(cx);
            crate::theme::init(cx);
            cx.set_reduce_motion(crate::platform::reduce_motion_enabled());

            // Sparkle only runs from a bundled release build (or when forced
            // via WAKU_FORCE_UPDATER=1); everywhere else the menu item is
            // omitted along with the updater itself.
            let updater = crate::updater::Updater::init();
            let updater_available = updater.is_some();
            cx.set_global(crate::updater::UpdaterState(updater));
            cx.on_action(|_: &CheckForUpdates, cx| {
                if let Some(updater) = &cx.global::<crate::updater::UpdaterState>().0 {
                    updater.check_for_updates();
                }
            });
            cx.on_action(|_: &About, _| crate::platform::show_about_panel());

            cx.bind_keys([
                KeyBinding::new("cmd-q", Quit, None),
                KeyBinding::new("cmd-w", CloseWindow, None),
                KeyBinding::new("cmd-n", NewSession, None),
                KeyBinding::new("cmd-o", NewProject, None),
                KeyBinding::new("cmd-,", OpenSettings, None),
                KeyBinding::new("cmd-b", ToggleSidebar, None),
                KeyBinding::new("cmd-shift-b", ToggleRightPanel, None),
                KeyBinding::new("cmd-k", ToggleCommandPalette, None),
                KeyBinding::new("cmd-alt-shift-f", ToggleFpsCounter, None),
                KeyBinding::new("cmd-[", NavigateBack, Some("Waku")),
                KeyBinding::new("cmd-]", NavigateForward, Some("Waku")),
                KeyBinding::new("cmd-l", FocusComposer, None),
                KeyBinding::new("cmd-/", ToggleModelPicker, None),
                KeyBinding::new("cmd-u", ToggleUsagePanel, None),
                KeyBinding::new("cmd-s", SaveFile, None),
                KeyBinding::new("escape", CancelTurn, Some("Waku")),
                KeyBinding::new("cmd-c", CopySelection, Some("Waku")),
                // Find and replace in the right panel's file editor, on the
                // conventional VS Code bindings. `cmd-g` cycles matches from
                // the editor without moving focus to the bar.
                KeyBinding::new("cmd-f", OpenFind, Some("Waku")),
                KeyBinding::new("cmd-alt-f", OpenFindReplace, Some("Waku")),
                KeyBinding::new("cmd-g", FindNext, Some("Waku")),
                KeyBinding::new("cmd-shift-g", FindPrevious, Some("Waku")),
                // Scoped to the editor pane: escape closes the bar there and
                // falls through to CancelTurn anywhere else.
                KeyBinding::new("escape", CloseFind, Some("FileEditorPane")),
                KeyBinding::new("cmd-alt-c", ToggleFindCaseSensitive, Some("FileEditorPane")),
                KeyBinding::new("cmd-alt-w", ToggleFindWholeWord, Some("FileEditorPane")),
                KeyBinding::new("cmd-alt-r", ToggleFindRegex, Some("FileEditorPane")),
                KeyBinding::new("shift-enter", FindPrevious, Some("FindBar")),
                KeyBinding::new("cmd-alt-enter", ReplaceAllMatches, Some("FindBar")),
                // Browser surface. Deeper than "Waku", so while focus is on the
                // page or its address bar the browser reads ⌘L/⌘R/⌘[/⌘]/Esc
                // the way every macOS browser does; the same keys elsewhere
                // keep their app meanings. The clipboard trio is rebound
                // because GPUI's window view claims key equivalents before
                // AppKit can walk the responder chain into the webview.
                KeyBinding::new("cmd-l", FocusBrowserAddress, Some("Browser")),
                KeyBinding::new("cmd-r", BrowserReload, Some("Browser")),
                KeyBinding::new("cmd-shift-r", BrowserHardReload, Some("Browser")),
                KeyBinding::new("cmd-[", BrowserBack, Some("Browser")),
                KeyBinding::new("cmd-]", BrowserForward, Some("Browser")),
                KeyBinding::new("escape", BrowserStop, Some("Browser")),
                KeyBinding::new("cmd-alt-i", BrowserDevtools, Some("Browser")),
                KeyBinding::new("cmd-c", WebviewCopy, Some("Browser")),
                KeyBinding::new("cmd-x", WebviewCut, Some("Browser")),
                KeyBinding::new("cmd-v", WebviewPaste, Some("Browser")),
                KeyBinding::new("cmd-a", WebviewSelectAll, Some("Browser")),
                KeyBinding::new("escape", BrowserAddressCancel, Some("BrowserAddress")),
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
                        waku
                    },
                )
                .expect("failed to open Waku window");

            cx.on_system_notification_response({
                let window = window;
                move |response, cx| {
                    let Some(session_id) = crate::app::task_id_from_notification_tag(&response.tag)
                    else {
                        return;
                    };
                    window
                        .update(cx, |waku, window, cx| {
                            waku.open_task_from_notification(session_id, cx);
                            window.activate_window();
                            cx.activate(true);
                        })
                        .ok();
                    cx.dismiss_system_notification(&response.tag);
                }
            });

            window
                .update(cx, |_, window, cx| {
                    crate::platform::configure_sidebar_material(
                        window,
                        crate::theme::Theme::current(cx).is_dark,
                    );
                    cx.activate(true);
                })
                .ok();

            set_app_menus(cx, updater_available);
        });
}

/// Rebuild the native menu bar in the active locale. GPUI menus own their
/// labels, so changing language must replace the model as well as redraw the
/// window.
pub(crate) fn set_app_menus(cx: &mut App, updater_available: bool) {
    cx.set_menus(vec![
        Menu {
            name: APP_NAME.into(),
            disabled: false,
            items: {
                let mut items = vec![MenuItem::action(tr!("menu.about", app = APP_NAME), About)];
                if updater_available {
                    items.push(MenuItem::action(
                        tr!("menu.check_for_updates"),
                        CheckForUpdates,
                    ));
                }
                items.push(MenuItem::separator());
                items.extend([
                    MenuItem::action(tr!("menu.settings"), OpenSettings),
                    MenuItem::separator(),
                    MenuItem::action(tr!("menu.quit", app = APP_NAME), Quit),
                ]);
                items
            },
        },
        Menu {
            name: tr!("menu.file").into(),
            disabled: false,
            items: vec![
                MenuItem::action(tr!("menu.new_task"), NewSession),
                MenuItem::action(tr!("menu.new_project"), NewProject),
            ],
        },
        Menu {
            name: tr!("menu.edit").into(),
            disabled: false,
            items: vec![
                MenuItem::action(tr!("menu.undo"), input::Undo),
                MenuItem::action(tr!("menu.redo"), input::Redo),
                MenuItem::separator(),
                MenuItem::action(tr!("menu.cut"), input::Cut),
                MenuItem::action(tr!("menu.copy"), input::Copy),
                MenuItem::action(tr!("menu.paste"), input::Paste),
                MenuItem::action(tr!("menu.select_all"), input::SelectAll),
            ],
        },
        Menu {
            name: tr!("menu.view").into(),
            disabled: false,
            items: vec![
                MenuItem::action(tr!("menu.command_palette"), ToggleCommandPalette),
                MenuItem::separator(),
                MenuItem::action(tr!("menu.toggle_sidebar"), ToggleSidebar),
                MenuItem::action(tr!("menu.toggle_right_panel"), ToggleRightPanel),
                MenuItem::action(tr!("menu.focus_composer"), FocusComposer),
                MenuItem::action(tr!("menu.toggle_model_picker"), ToggleModelPicker),
                MenuItem::action(tr!("menu.toggle_usage_panel"), ToggleUsagePanel),
            ],
        },
        Menu {
            name: tr!("menu.window").into(),
            disabled: false,
            items: vec![
                MenuItem::action(tr!("menu.toggle_fps_counter"), ToggleFpsCounter),
                MenuItem::action(tr!("menu.close_window"), CloseWindow),
            ],
        },
    ]);
}

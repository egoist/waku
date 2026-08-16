#![recursion_limit = "256"]

rust_i18n::i18n!("locales", fallback = "en");

// rust-i18n expands locale data in a proc macro, which Cargo does not always
// discover as an input when only a YAML file changes. Keep explicit source
// dependencies so the watcher rebuilds the translation registry itself.
const _LOCALE_SOURCES: [&str; 3] = [
    include_str!("../locales/app.yml"),
    include_str!("../locales/zh-CN.yml"),
    include_str!("../locales/ja.yml"),
];

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

mod analytics;
mod app;
mod assets;
mod browser;
mod computer_use;
pub mod daemon;
mod driver;
mod input;
mod md;
mod platform;
mod query;
mod review_diff;
mod terminal;
mod theme;
mod ui;
mod updater;

pub use waku_client::{
    automation, checkpoint, command_env, composer_complete, git_branch, git_commit, i18n, identity,
    model, model_catalog, persistence, projectless, skills, usage, usage_history, worktree,
};

use gpui::{
    App, Application, Bounds, KeyBinding, Menu, MenuItem, TitlebarOptions,
    WindowBackgroundAppearance, WindowBounds, WindowOptions, actions, point, px, size,
};

use crate::app::Waku;
use crate::identity::{APP_ID, APP_NAME};
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

const DEFAULT_WINDOW_WIDTH: f32 = 1380.0;
const DEFAULT_WINDOW_HEIGHT: f32 = 880.0;
const MIN_WINDOW_WIDTH: f32 = 980.0;
const MIN_WINDOW_HEIGHT: f32 = 680.0;
/// How much titlebar must stay on the display for the window to be dragged
/// back by hand.
const TITLEBAR_GRAB_WIDTH: f32 = 160.0;
const TITLEBAR_GRAB_HEIGHT: f32 = 22.0;

/// Reopen the main window where the user last left it, on the display it was
/// left on. GPUI window bounds are display-relative, so the persisted frame is
/// anchored by resolving the saved display UUID against the connected
/// displays — Zed's scheme — and `display_id` rides along in `WindowOptions`.
/// When that display is gone the same offsets re-anchor on the primary
/// display, and the origin is clamped so the titlebar stays grabbable after
/// any display change.
fn restored_window_placement(cx: &App) -> (WindowBounds, Option<gpui::DisplayId>) {
    let centered = |cx: &App| {
        (
            WindowBounds::Windowed(Bounds::centered(
                None,
                size(px(DEFAULT_WINDOW_WIDTH), px(DEFAULT_WINDOW_HEIGHT)),
                cx,
            )),
            None,
        )
    };
    let Some(saved) = crate::persistence::load_window_state().filter(|saved| {
        [saved.x, saved.y, saved.width, saved.height]
            .iter()
            .all(|value| value.is_finite())
    }) else {
        return centered(cx);
    };
    let display = saved.display.and_then(|uuid| {
        cx.displays()
            .into_iter()
            .find(|display| display.uuid().ok() == Some(uuid))
    });
    let display_id = display.as_ref().map(|display| display.id());
    let Some(anchor) = display.or_else(|| cx.primary_display()) else {
        return centered(cx);
    };
    let anchor_size = anchor.bounds().size;
    let width = saved.width.max(MIN_WINDOW_WIDTH);
    let height = saved.height.max(MIN_WINDOW_HEIGHT);
    let x = saved.x.clamp(
        TITLEBAR_GRAB_WIDTH - width,
        (f32::from(anchor_size.width) - TITLEBAR_GRAB_WIDTH).max(0.0),
    );
    let y = saved.y.clamp(
        0.0,
        (f32::from(anchor_size.height) - TITLEBAR_GRAB_HEIGHT).max(0.0),
    );
    let bounds = Bounds::new(point(px(x), px(y)), size(px(width), px(height)));
    let window_bounds = if saved.maximized {
        WindowBounds::Maximized(bounds)
    } else {
        WindowBounds::Windowed(bounds)
    };
    (window_bounds, display_id)
}

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

pub fn run() {
    let daemon = crate::daemon::start_process()
        .unwrap_or_else(|error| panic!("failed to start Waku daemon: {error:#}"));
    gpui_platform::application()
        .with_assets(crate::assets::Assets)
        .with_main_window_reopen()
        .run(move |cx: &mut App| {
            // Linux uses this for Wayland app_id/X11 WM_CLASS and notification
            // attribution. Other platforms also benefit from one stable
            // process identity.
            cx.set_app_identity(APP_ID, APP_NAME);
            crate::assets::register_fonts(cx).expect("failed to register bundled fonts");
            crate::input::init(cx);
            crate::ui::menu::init(cx);
            crate::app::init_composer_autocomplete(cx);
            crate::app::init_settings_keys(cx);
            crate::app::init_command_palette(cx);
            crate::app::init_commit_dialog_keys(cx);
            crate::app::init_automation_delete_dialog_keys(cx);
            crate::app::init_image_preview_keys(cx);
            crate::app::init_sidebar_keys(cx);
            crate::app::init_skills_keys(cx);
            crate::theme::init(cx);
            crate::platform::init_reduce_motion(cx);

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
                // `secondary` is Command on macOS and Control elsewhere.
                KeyBinding::new("secondary-q", Quit, None),
                KeyBinding::new("secondary-w", CloseWindow, None),
                KeyBinding::new("secondary-n", NewSession, None),
                KeyBinding::new("secondary-o", NewProject, None),
                KeyBinding::new("secondary-,", OpenSettings, None),
                KeyBinding::new("secondary-b", ToggleSidebar, None),
                KeyBinding::new("secondary-shift-b", ToggleRightPanel, None),
                KeyBinding::new("secondary-k", ToggleCommandPalette, None),
                KeyBinding::new("secondary-alt-shift-f", ToggleFpsCounter, None),
                KeyBinding::new("secondary-[", NavigateBack, Some("Waku")),
                KeyBinding::new("secondary-]", NavigateForward, Some("Waku")),
                KeyBinding::new("secondary-l", FocusComposer, None),
                KeyBinding::new("secondary-/", ToggleModelPicker, None),
                KeyBinding::new("secondary-u", ToggleUsagePanel, None),
                KeyBinding::new("secondary-s", SaveFile, None),
                KeyBinding::new("escape", CancelTurn, Some("Waku")),
                KeyBinding::new("secondary-c", CopySelection, Some("Waku")),
                // Find and replace in the right panel's file editor, on the
                // conventional VS Code bindings. The primary shortcut + G cycles matches from
                // the editor without moving focus to the bar.
                KeyBinding::new("secondary-f", OpenFind, Some("Waku")),
                KeyBinding::new("secondary-alt-f", OpenFindReplace, Some("Waku")),
                KeyBinding::new("secondary-g", FindNext, Some("Waku")),
                KeyBinding::new("secondary-shift-g", FindPrevious, Some("Waku")),
                // Scoped to the editor pane: escape closes the bar there and
                // falls through to CancelTurn anywhere else.
                KeyBinding::new("escape", CloseFind, Some("FileEditorPane")),
                KeyBinding::new(
                    "secondary-alt-c",
                    ToggleFindCaseSensitive,
                    Some("FileEditorPane"),
                ),
                KeyBinding::new(
                    "secondary-alt-w",
                    ToggleFindWholeWord,
                    Some("FileEditorPane"),
                ),
                KeyBinding::new("secondary-alt-r", ToggleFindRegex, Some("FileEditorPane")),
                KeyBinding::new("shift-enter", FindPrevious, Some("FindBar")),
                KeyBinding::new("secondary-alt-enter", ReplaceAllMatches, Some("FindBar")),
                // Browser surface. Deeper than "Waku", so while focus is on the
                // page or its address bar the browser reads the platform's
                // conventional navigation shortcuts; the same keys elsewhere
                // keep their app meanings. The clipboard trio is rebound
                // because GPUI's window view claims key equivalents before
                // AppKit can walk the responder chain into the webview.
                KeyBinding::new("secondary-l", FocusBrowserAddress, Some("Browser")),
                KeyBinding::new("secondary-r", BrowserReload, Some("Browser")),
                KeyBinding::new("secondary-shift-r", BrowserHardReload, Some("Browser")),
                KeyBinding::new("secondary-[", BrowserBack, Some("Browser")),
                KeyBinding::new("secondary-]", BrowserForward, Some("Browser")),
                KeyBinding::new("escape", BrowserStop, Some("Browser")),
                KeyBinding::new("secondary-alt-i", BrowserDevtools, Some("Browser")),
                KeyBinding::new("secondary-c", WebviewCopy, Some("Browser")),
                KeyBinding::new("secondary-x", WebviewCut, Some("Browser")),
                KeyBinding::new("secondary-v", WebviewPaste, Some("Browser")),
                KeyBinding::new("secondary-a", WebviewSelectAll, Some("Browser")),
                KeyBinding::new("escape", BrowserAddressCancel, Some("BrowserAddress")),
            ]);

            cx.on_action(|_: &Quit, cx| cx.quit());

            // Unlike AppKit, Linux has no Dock activation path that can
            // restore a hidden last window. Follow Zed's GPUI precedent and
            // terminate when the final window closes.
            #[cfg(not(target_os = "macos"))]
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();

            let (window_bounds, display_id) = restored_window_placement(cx);
            let window = cx
                .open_window(
                    WindowOptions {
                        titlebar: Some(TitlebarOptions {
                            title: Some(APP_NAME.into()),
                            appears_transparent: cfg!(target_os = "macos"),
                            traffic_light_position: cfg!(target_os = "macos")
                                .then(|| point(px(16.0), px(17.0))),
                        }),
                        // Waku moves its custom macOS titlebar explicitly. Keep
                        // the NSWindow movable so native controls and Window-menu
                        // tiling remain enabled.
                        is_movable: true,
                        app_owns_titlebar_drag: cfg!(target_os = "macos"),
                        window_background: if cfg!(target_os = "macos") {
                            WindowBackgroundAppearance::Blurred
                        } else {
                            WindowBackgroundAppearance::Opaque
                        },
                        app_id: Some(APP_ID.to_owned()),
                        // GPUI defaults to compositor/server decorations. If a
                        // Wayland compositor declines them, it reports the
                        // client fallback and Waku renders that frame itself.
                        #[cfg(target_os = "linux")]
                        icon: crate::platform::linux_app_icon(),
                        window_bounds: Some(window_bounds),
                        display_id,
                        window_min_size: Some(size(px(MIN_WINDOW_WIDTH), px(MIN_WINDOW_HEIGHT))),
                        ..Default::default()
                    },
                    move |window, cx| {
                        crate::platform::configure_main_window_close_behavior(window, cx);
                        let waku = Waku::new(window, cx, daemon);
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

use gpui::Window;

// crates.io GPUI 0.2.2 leaves `start_window_move` as a no-op on macOS.
#[cfg(target_os = "macos")]
pub fn start_window_move(window: &Window) {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSView};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let Ok(handle) = HasWindowHandle::window_handle(window) else {
        return;
    };
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return;
    };
    let Some(main_thread) = MainThreadMarker::new() else {
        return;
    };

    // GPUI owns this NSView for the lifetime of `window`, and AppKit access is
    // guarded by the main-thread marker above.
    unsafe {
        let view = handle.ns_view.cast::<NSView>().as_ref();
        let Some(native_window) = view.window() else {
            return;
        };
        let app = NSApplication::sharedApplication(main_thread);
        let Some(event) = app.currentEvent() else {
            return;
        };
        native_window.performWindowDragWithEvent(&event);
    }
}

#[cfg(not(target_os = "macos"))]
pub fn start_window_move(window: &Window) {
    window.start_window_move();
}

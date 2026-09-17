// A GUI build must not open a console window behind the app. Debug builds
// keep the console subsystem so `cargo run` and the dev watcher still see
// stdout and panics.
#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

fn main() {
    // GPUI's headless backend has no desktop window. Reject it before
    // waku::run() starts the companion daemon and enters the event loop.
    #[cfg(target_os = "linux")]
    if gpui::guess_compositor() == "Headless" {
        eprintln!(
            "Waku requires an X11 or Wayland display. Launch it from a graphical session \
             with DISPLAY or WAYLAND_DISPLAY set and ZED_HEADLESS unset. \
             For a headless server, run waku-daemon instead."
        );
        std::process::exit(1);
    }
    waku::run();
}

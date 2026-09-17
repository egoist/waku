#![cfg(target_os = "linux")]

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn launch(display: Option<&str>, wayland: Option<&str>, headless: bool) -> String {
    let mut command = Command::new(env!("CARGO_BIN_EXE_waku"));
    command
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .env_remove("ZED_HEADLESS")
        // An incomplete remote-daemon configuration stops the next startup
        // stage without starting a daemon or connecting to a real display.
        .env("WAKU_DAEMON_ADDRESS", "ws://127.0.0.1:1")
        .env_remove("WAKU_DAEMON_TOKEN")
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    if let Some(value) = display {
        command.env("DISPLAY", value);
    }
    if let Some(value) = wayland {
        command.env("WAYLAND_DISPLAY", value);
    }
    if headless {
        command.env("ZED_HEADLESS", "1");
    }
    let mut child = command.spawn().expect("start desktop executable");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if child.try_wait().expect("poll desktop").is_some() {
            break;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("desktop did not reject the startup environment promptly");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let output = child.wait_with_output().expect("read desktop diagnostic");
    assert!(!output.status.success());
    String::from_utf8(output.stderr).expect("UTF-8 diagnostic")
}

#[test]
fn headless_desktop_is_rejected_before_daemon_startup() {
    for (display, wayland, headless) in [
        (None, None, false),
        (Some(""), Some(""), false),
        (Some(":99"), Some("wayland-99"), true),
    ] {
        let diagnostic = launch(display, wayland, headless);
        assert!(diagnostic.contains("Waku requires an X11 or Wayland display"));
        assert!(!diagnostic.contains("failed to start Waku daemon"));
    }
}

#[test]
fn configured_displays_reach_the_existing_daemon_startup() {
    for (display, wayland) in [
        (Some(":99"), None),
        (None, Some("wayland-99")),
        (Some(":99"), Some("")),
    ] {
        let diagnostic = launch(display, wayland, false);
        assert!(diagnostic.contains("WAKU_DAEMON_TOKEN is missing"));
        assert!(!diagnostic.contains("Waku requires an X11 or Wayland display"));
    }
}

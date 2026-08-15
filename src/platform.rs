use gpui::Window;

#[cfg(target_os = "macos")]
pub fn show_about_panel() {
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSApplication;

    let Some(main_thread) = MainThreadMarker::new() else {
        return;
    };
    NSApplication::sharedApplication(main_thread).orderFrontStandardAboutPanel(None);
}

#[cfg(not(target_os = "macos"))]
pub fn show_about_panel() {}

/// Register embedded font data with CoreText at process scope. GPUI's
/// `add_fonts` only feeds its private font-kit source, which CoreText cascade
/// matching cannot see — and it refuses symbols-only faces outright (fonts
/// with no 'm' glyph). Fonts referenced through `FontFallbacks` therefore
/// must be registered here instead.
#[cfg(target_os = "macos")]
pub fn register_fonts_with_coretext(fonts: &[&'static [u8]]) -> anyhow::Result<()> {
    use std::ffi::c_void;

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGDataProviderCreateWithData(
            info: *mut c_void,
            data: *const u8,
            size: usize,
            release_callback: *const c_void,
        ) -> *mut c_void;
        fn CGFontCreateWithDataProvider(provider: *mut c_void) -> *mut c_void;
        fn CGDataProviderRelease(provider: *mut c_void);
        fn CGFontRelease(font: *mut c_void);
    }
    #[link(name = "CoreText", kind = "framework")]
    unsafe extern "C" {
        fn CTFontManagerRegisterGraphicsFont(font: *mut c_void, error: *mut *mut c_void) -> bool;
    }

    for (index, data) in fonts.iter().enumerate() {
        unsafe {
            let provider = CGDataProviderCreateWithData(
                std::ptr::null_mut(),
                data.as_ptr(),
                data.len(),
                std::ptr::null(),
            );
            anyhow::ensure!(!provider.is_null(), "font {index}: not a readable buffer");
            let font = CGFontCreateWithDataProvider(provider);
            CGDataProviderRelease(provider);
            anyhow::ensure!(!font.is_null(), "font {index}: not a valid font");
            let registered = CTFontManagerRegisterGraphicsFont(font, std::ptr::null_mut());
            CGFontRelease(font);
            anyhow::ensure!(registered, "font {index}: CoreText registration failed");
        }
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
pub fn register_fonts_with_coretext(_: &[&'static [u8]]) -> anyhow::Result<()> {
    Ok(())
}

#[cfg(target_os = "macos")]
pub fn init_reduce_motion(cx: &mut gpui::App) {
    use objc2_app_kit::NSWorkspace;

    cx.set_reduce_motion(NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceMotion());
}

#[cfg(target_os = "linux")]
pub fn init_reduce_motion(cx: &mut gpui::App) {
    if let Ok(value) = std::env::var("WAKU_REDUCE_MOTION")
        && let Some(enabled) = parse_boolean_setting(&value)
    {
        cx.set_reduce_motion(enabled);
        return;
    }

    // GNOME exposes its animation preference through GSettings. Resolve it
    // once off the UI thread; frames only read GPUI's in-memory flag.
    cx.spawn(async move |cx| {
        let enabled = cx
            .background_executor()
            .spawn(async move { linux_reduce_motion_enabled() })
            .await;
        cx.update(|cx| cx.set_reduce_motion(enabled));
    })
    .detach();
}

#[cfg(target_os = "linux")]
fn linux_reduce_motion_enabled() -> bool {
    std::process::Command::new("gsettings")
        .args(["get", "org.gnome.desktop.interface", "enable-animations"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .and_then(|value| parse_boolean_setting(&value))
        .is_some_and(|animations_enabled| !animations_enabled)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn init_reduce_motion(_: &mut gpui::App) {}

#[cfg(target_os = "linux")]
fn parse_boolean_setting(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

/// Deliver an audible macOS notification. GPUI owns the notification-center
/// delegate (and therefore click responses); Waku only supplies content here
/// because GPUI's generic payload does not currently expose a sound field.
#[cfg(target_os = "macos")]
pub fn show_task_notification(tag: &str, title: &str, body: &str, _: &gpui::App) {
    use block2::RcBlock;
    use objc2::runtime::Bool;
    use objc2_foundation::{NSBundle, NSError, NSString};
    use objc2_user_notifications::{
        UNAuthorizationOptions, UNMutableNotificationContent, UNNotificationRequest,
        UNNotificationSound, UNUserNotificationCenter,
    };

    // UserNotifications raises an Objective-C exception for an executable
    // outside an application bundle, including unit tests and `cargo run`.
    if NSBundle::mainBundle().bundleIdentifier().is_none() {
        return;
    }

    let tag = tag.to_owned();
    let title = title.to_owned();
    let body = body.to_owned();
    let authorization = RcBlock::new(move |granted: Bool, _error: *mut NSError| {
        if !granted.as_bool() {
            return;
        }

        let content = UNMutableNotificationContent::new();
        content.setTitle(&NSString::from_str(&title));
        content.setBody(&NSString::from_str(&body));
        content.setSound(Some(&UNNotificationSound::defaultSound()));

        // A nil trigger delivers immediately. The stable task tag replaces an
        // older completion banner for the same task and comes back on click.
        let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
            &NSString::from_str(&tag),
            &content,
            None,
        );
        UNUserNotificationCenter::currentNotificationCenter()
            .addNotificationRequest_withCompletionHandler(&request, None);
    });
    UNUserNotificationCenter::currentNotificationCenter()
        .requestAuthorizationWithOptions_completionHandler(
            UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound,
            &authorization,
        );
}

#[cfg(not(target_os = "macos"))]
pub fn show_task_notification(tag: &str, title: &str, body: &str, cx: &gpui::App) {
    cx.show_system_notification(gpui::SystemNotification {
        tag: tag.to_owned().into(),
        title: title.to_owned().into(),
        body: body.to_owned().into(),
        actions: Vec::new(),
    });
}

#[cfg(target_os = "macos")]
pub fn load_app_icon_for_bundle_id(bundle_id: &str) -> Option<std::sync::Arc<gpui::Image>> {
    use objc2_app_kit::{NSBitmapImageFileType, NSBitmapImageRep, NSWorkspace};
    use objc2_foundation::{NSDictionary, NSSize, NSString};

    let bundle_id = NSString::from_str(bundle_id);
    let workspace = NSWorkspace::sharedWorkspace();
    let application_url = workspace.URLForApplicationWithBundleIdentifier(&bundle_id)?;
    let application_path = application_url.path()?;
    let image = workspace.iconForFile(&application_path);
    image.setSize(NSSize::new(32.0, 32.0));
    let tiff_data = image.TIFFRepresentation()?;
    let bitmap_rep = NSBitmapImageRep::imageRepWithData(&tiff_data)?;
    let properties = NSDictionary::new();
    let png_data = unsafe {
        bitmap_rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &properties)
    }?;
    let bytes = unsafe { png_data.as_bytes_unchecked() };
    (!bytes.is_empty()).then(|| {
        std::sync::Arc::new(gpui::Image::from_bytes(
            gpui::ImageFormat::Png,
            bytes.to_vec(),
        ))
    })
}

#[cfg(not(target_os = "macos"))]
pub fn load_app_icon_for_bundle_id(_: &str) -> Option<std::sync::Arc<gpui::Image>> {
    None
}

/// Select `path` in the platform file manager. GPUI dispatches Linux portal
/// and subprocess work away from the UI thread.
pub fn reveal_in_file_manager(path: &std::path::Path, cx: &gpui::App) {
    cx.reveal_path(path);
}

/// Open `path` with its default application — a document in its editor.
pub fn open_with_default_app(path: &std::path::Path, cx: &gpui::App) {
    cx.open_with_system(path);
}

/// Move `path` to the Trash, recoverably. Errors surface to the caller so the
/// UI can say why nothing moved.
#[cfg(target_os = "macos")]
pub fn trash_item(path: &std::path::Path) -> Result<(), String> {
    use objc2_foundation::{NSFileManager, NSString, NSURL};

    let url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
    NSFileManager::defaultManager()
        .trashItemAtURL_resultingItemURL_error(&url, None)
        .map_err(|error| error.localizedDescription().to_string())
}

#[cfg(target_os = "linux")]
pub fn trash_item(path: &std::path::Path) -> Result<(), String> {
    trash::delete(path).map_err(|error| error.to_string())
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn trash_item(_: &std::path::Path) -> Result<(), String> {
    Err("moving items to Trash is not supported on this platform".to_owned())
}

/// Decode the embedded desktop icon once. X11 consumes the RGBA pixels from
/// `WindowOptions`; Wayland associates the window through `app_id` and its
/// installed desktop entry.
#[cfg(target_os = "linux")]
pub fn linux_app_icon() -> Option<std::sync::Arc<image::RgbaImage>> {
    static ICON: std::sync::LazyLock<Option<std::sync::Arc<image::RgbaImage>>> =
        std::sync::LazyLock::new(|| {
            image::load_from_memory(include_bytes!("../website/public/app-icon.png"))
                .ok()
                .map(|image| std::sync::Arc::new(image.into_rgba8()))
        });
    ICON.clone()
}

/// A compact shortcut label for the platform's primary GUI modifier.
pub const fn primary_shortcut<'a>(macos: &'a str, other: &'a str) -> &'a str {
    if cfg!(target_os = "macos") {
        macos
    } else {
        other
    }
}

/// Keep Waku's single main window alive when the user closes it. This preserves
/// the current session and lets a Dock activation reveal the same GPUI window.
#[cfg(target_os = "macos")]
pub fn configure_main_window_close_behavior(window: &Window, cx: &gpui::App) {
    window.on_window_should_close(cx, |window, _| {
        hide_window(window);
        false
    });
}

#[cfg(not(target_os = "macos"))]
pub fn configure_main_window_close_behavior(_: &Window, _: &gpui::App) {}

#[cfg(target_os = "macos")]
pub fn hide_window(window: &mut Window) {
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSView;
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let Ok(handle) = HasWindowHandle::window_handle(window) else {
        return;
    };
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return;
    };
    let Some(_main_thread) = MainThreadMarker::new() else {
        return;
    };

    // GPUI owns this view and its NSWindow. AppKit access stays on the main
    // thread, and orderOut hides without triggering GPUI's close callback.
    unsafe {
        let view = handle.ns_view.cast::<NSView>().as_ref();
        if let Some(native_window) = view.window() {
            native_window.orderOut(None);
        }
    }
}

#[cfg(not(target_os = "macos"))]
pub fn hide_window(window: &mut Window) {
    window.remove_window();
}

#[cfg(target_os = "macos")]
thread_local! {
    static SIDEBAR_TINT_VIEW: std::cell::RefCell<Option<objc2::rc::Retained<objc2_app_kit::NSView>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(target_os = "macos")]
const SIDEBAR_WIDTH: f64 = 252.0;

pub fn start_window_move(window: &Window) {
    window.start_window_move();
}

/// Perform the platform's titlebar double-click action. GPUI delegates this
/// to the user's system preference on macOS, while Linux client decorations
/// must toggle maximize explicitly.
pub fn titlebar_double_click(window: &Window) {
    #[cfg(target_os = "macos")]
    window.titlebar_double_click();

    #[cfg(not(target_os = "macos"))]
    if window.window_controls().maximize && window.is_resizable() {
        window.zoom_window();
    }
}

/// Match Cursor's macOS glass window stack without asking GPUI's transparent
/// Metal target to blend two translucent quads. The semantic tint is a native
/// view above active Sidebar vibrancy; GPUI paints clear sidebar chrome and one
/// translucent interaction layer above it.
#[cfg(target_os = "macos")]
pub fn configure_sidebar_material(window: &Window, dark: bool) {
    use objc2::{MainThreadMarker, MainThreadOnly};
    use objc2_app_kit::{
        NSAutoresizingMaskOptions, NSColor, NSView, NSVisualEffectBlendingMode,
        NSVisualEffectMaterial, NSVisualEffectState, NSVisualEffectView, NSWindowOrderingMode,
    };
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

    // GPUI owns the view hierarchy and creates the effect view before the
    // root entity is installed. We only adjust public AppKit properties.
    unsafe {
        let view = handle.ns_view.cast::<NSView>().as_ref();
        let Some(native_window) = view.window() else {
            return;
        };
        let background = if dark {
            NSColor::colorWithSRGBRed_green_blue_alpha(0.0, 0.0, 0.0, 0.25)
        } else {
            NSColor::colorWithSRGBRed_green_blue_alpha(1.0, 1.0, 1.0, 0.0)
        };
        native_window.setBackgroundColor(Some(&background));

        let Some(content_view) = native_window.contentView() else {
            return;
        };

        let mut configured_effect = false;
        for subview in content_view.subviews().iter() {
            let Some(effect_view) = subview.downcast_ref::<NSVisualEffectView>() else {
                continue;
            };
            effect_view.setMaterial(NSVisualEffectMaterial::Sidebar);
            effect_view.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
            effect_view.setState(NSVisualEffectState::Active);
            configured_effect = true;
        }
        if !configured_effect {
            return;
        }

        let channel = if dark { 0x18 } else { 0xF3 } as f64 / 255.0;
        let tint = NSColor::colorWithSRGBRed_green_blue_alpha(channel, channel, channel, 0.92);

        SIDEBAR_TINT_VIEW.with_borrow_mut(|slot| {
            let needs_new_view = slot.as_ref().is_none_or(|tint_view| {
                tint_view
                    .window()
                    .as_deref()
                    .is_none_or(|window| !std::ptr::eq(window, native_window.as_ref()))
            });
            if needs_new_view {
                let mut frame = content_view.bounds();
                frame.size.width = SIDEBAR_WIDTH;
                let tint_view = NSView::initWithFrame(NSView::alloc(main_thread), frame);
                tint_view.setAutoresizingMask(NSAutoresizingMaskOptions::ViewHeightSizable);
                tint_view.setWantsLayer(true);
                content_view.addSubview_positioned_relativeTo(
                    &tint_view,
                    NSWindowOrderingMode::Below,
                    Some(view),
                );
                *slot = Some(tint_view);
            }

            if let Some(layer) = slot.as_ref().and_then(|tint_view| tint_view.layer()) {
                layer.setBackgroundColor(Some(&tint.CGColor()));
            }
        });
    }
}

#[cfg(not(target_os = "macos"))]
pub fn configure_sidebar_material(_: &Window, _: bool) {}

#[cfg(target_os = "macos")]
pub fn set_sidebar_material_width(window: &Window, width: f32) {
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSView;
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let Ok(handle) = HasWindowHandle::window_handle(window) else {
        return;
    };
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return;
    };
    let Some(_main_thread) = MainThreadMarker::new() else {
        return;
    };

    unsafe {
        let view = handle.ns_view.cast::<NSView>().as_ref();
        let Some(native_window) = view.window() else {
            return;
        };
        SIDEBAR_TINT_VIEW.with_borrow(|slot| {
            let Some(tint_view) = slot.as_ref().filter(|tint_view| {
                tint_view
                    .window()
                    .as_deref()
                    .is_some_and(|window| std::ptr::eq(window, native_window.as_ref()))
            }) else {
                return;
            };
            let mut frame = tint_view.frame();
            frame.size.width = width.into();
            tint_view.setFrame(frame);
        });
    }
}

#[cfg(not(target_os = "macos"))]
pub fn set_sidebar_material_width(_: &Window, _: f32) {}

/// Follow macOS when `dark` is `None`, otherwise force the native titlebar,
/// traffic lights, menus, and vibrancy to the selected appearance.
#[cfg(target_os = "macos")]
pub fn set_window_appearance(window: &Window, dark: Option<bool>) {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{
        NSAppearance, NSAppearanceCustomization, NSAppearanceNameAqua, NSAppearanceNameDarkAqua,
        NSView,
    };
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let Ok(handle) = HasWindowHandle::window_handle(window) else {
        return;
    };
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return;
    };
    let Some(_main_thread) = MainThreadMarker::new() else {
        return;
    };

    unsafe {
        let view = handle.ns_view.cast::<NSView>().as_ref();
        let Some(native_window) = view.window() else {
            return;
        };
        let appearance = dark.and_then(|dark| {
            NSAppearance::appearanceNamed(if dark {
                NSAppearanceNameDarkAqua
            } else {
                NSAppearanceNameAqua
            })
        });
        native_window.setAppearance(appearance.as_deref());
    }
}

#[cfg(not(target_os = "macos"))]
pub fn set_window_appearance(_: &Window, _: Option<bool>) {}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::parse_boolean_setting;

    #[test]
    fn boolean_desktop_settings_are_parsed_case_insensitively() {
        assert_eq!(parse_boolean_setting(" true\n"), Some(true));
        assert_eq!(parse_boolean_setting("OFF"), Some(false));
        assert_eq!(parse_boolean_setting("default"), None);
    }

    #[test]
    fn embedded_linux_icon_decodes_at_desktop_size() {
        let icon = super::linux_app_icon().expect("embedded PNG should decode");

        assert_eq!(icon.dimensions(), (256, 256));
    }
}

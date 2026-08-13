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
pub fn reduce_motion_enabled() -> bool {
    use objc2_app_kit::NSWorkspace;

    NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceMotion()
}

#[cfg(not(target_os = "macos"))]
pub fn reduce_motion_enabled() -> bool {
    false
}

/// Deliver an audible macOS notification. GPUI owns the notification-center
/// delegate (and therefore click responses); Waku only supplies content here
/// because GPUI's generic payload does not currently expose a sound field.
#[cfg(target_os = "macos")]
pub fn show_task_notification(tag: &str, title: &str, body: &str) {
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
pub fn show_task_notification(_: &str, _: &str, _: &str) {}

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

/// Select `path` in a Finder window.
#[cfg(target_os = "macos")]
pub fn reveal_in_finder(path: &std::path::Path) {
    use objc2_app_kit::NSWorkspace;
    use objc2_foundation::{NSArray, NSString, NSURL};

    let url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
    NSWorkspace::sharedWorkspace()
        .activateFileViewerSelectingURLs(&NSArray::from_retained_slice(&[url]));
}

#[cfg(not(target_os = "macos"))]
pub fn reveal_in_finder(_: &std::path::Path) {}

/// Open `path` with its default application — a document in its editor.
#[cfg(target_os = "macos")]
pub fn open_with_default_app(path: &std::path::Path) {
    use objc2_app_kit::NSWorkspace;
    use objc2_foundation::{NSString, NSURL};

    let url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
    NSWorkspace::sharedWorkspace().openURL(&url);
}

#[cfg(not(target_os = "macos"))]
pub fn open_with_default_app(_: &std::path::Path) {}

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

#[cfg(not(target_os = "macos"))]
pub fn trash_item(path: &std::path::Path) -> Result<(), String> {
    std::fs::remove_dir_all(path).map_err(|error| error.to_string())
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

/// One row of a native context menu, in the order it should appear.
pub struct NativeMenuItem {
    pub label: String,
    pub enabled: bool,
    pub checked: bool,
    pub separator: bool,
}

#[cfg(target_os = "macos")]
mod native_menu {
    use std::cell::Cell;

    use gpui::{Pixels, Point, Window};
    use objc2::rc::Retained;
    use objc2::runtime::{NSObject, NSObjectProtocol};
    use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
    use objc2_app_kit::{
        NSControlStateValueOff, NSControlStateValueOn, NSMenu, NSMenuItem, NSView,
    };
    use objc2_foundation::{NSPoint, NSString};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    use super::NativeMenuItem;

    define_class!(
        // SAFETY: `NSObject` has no subclassing requirements and `MenuTarget`
        // does not implement `Drop`.
        #[unsafe(super(NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "WakuContextMenuTarget"]
        #[ivars = Cell<isize>]
        struct MenuTarget;

        unsafe impl NSObjectProtocol for MenuTarget {}

        impl MenuTarget {
            // AppKit sends this to the chosen item's target, and sends nothing
            // at all when the menu is dismissed — which leaves the tag at -1.
            #[unsafe(method(wakuContextMenuItemSelected:))]
            fn item_selected(&self, item: &NSMenuItem) {
                self.ivars().set(item.tag());
            }
        }
    );

    impl MenuTarget {
        fn new(main_thread: MainThreadMarker) -> Retained<Self> {
            let this = Self::alloc(main_thread).set_ivars(Cell::new(-1));
            unsafe { msg_send![super(this), init] }
        }
    }

    /// The window's native surface, captured while GPUI hands us a `Window` so
    /// the menu itself can be popped later, from outside any GPUI borrow.
    pub struct NativeContextMenu {
        view: Retained<NSView>,
    }

    /// Capture that surface, or `None` when there is none — a headless test
    /// window. The caller then falls back to the in-app menu card.
    pub fn prepare_context_menu(window: &Window) -> Option<NativeContextMenu> {
        let handle = HasWindowHandle::window_handle(window).ok()?;
        let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
            return None;
        };
        MainThreadMarker::new()?;

        // GPUI owns this view; we only read its geometry and anchor a menu to
        // it.
        let view = unsafe { Retained::retain(handle.ns_view.cast::<NSView>().as_ptr())? };
        Some(NativeContextMenu { view })
    }

    impl NativeContextMenu {
        /// Pop the menu at `position`, in GPUI's window coordinates, and block
        /// on AppKit's tracking loop until it closes. Returns the index of the
        /// chosen item within `items`.
        ///
        /// Must be called with no GPUI borrow held: the tracking loop keeps the
        /// run loop turning, so GPUI redraws and its own timers still fire
        /// while the menu is up.
        pub fn show(&self, position: Point<Pixels>, items: &[NativeMenuItem]) -> Option<usize> {
            let main_thread = MainThreadMarker::new()?;
            let target = MenuTarget::new(main_thread);
            let menu = NSMenu::new(main_thread);
            // Enablement is ours to decide; AppKit's automatic validation
            // would grey out every item, since a Rust target implements no
            // validation protocol.
            menu.setAutoenablesItems(false);

            for (index, item) in items.iter().enumerate() {
                if item.separator {
                    menu.addItem(&NSMenuItem::separatorItem(main_thread));
                    continue;
                }
                let entry = unsafe {
                    NSMenuItem::initWithTitle_action_keyEquivalent(
                        NSMenuItem::alloc(main_thread),
                        &NSString::from_str(&item.label),
                        Some(sel!(wakuContextMenuItemSelected:)),
                        &NSString::from_str(""),
                    )
                };
                unsafe { entry.setTarget(Some(&target)) };
                entry.setTag(index as isize);
                entry.setEnabled(item.enabled);
                entry.setState(if item.checked {
                    NSControlStateValueOn
                } else {
                    NSControlStateValueOff
                });
                menu.addItem(&entry);
            }

            // GPUI's coordinates start at the window's top-left; an unflipped
            // AppKit view measures up from its bottom-left.
            let height = self.view.bounds().size.height;
            let y = f64::from(f32::from(position.y));
            let y = if self.view.isFlipped() { y } else { height - y };
            let location = NSPoint::new(f64::from(f32::from(position.x)), y);
            menu.popUpMenuPositioningItem_atLocation_inView(None, location, Some(&self.view));

            usize::try_from(target.ivars().get()).ok()
        }
    }
}

#[cfg(target_os = "macos")]
pub use native_menu::{NativeContextMenu, prepare_context_menu};

#[cfg(not(target_os = "macos"))]
pub struct NativeContextMenu {
    _private: (),
}

#[cfg(not(target_os = "macos"))]
pub fn prepare_context_menu(_: &Window) -> Option<NativeContextMenu> {
    None
}

#[cfg(not(target_os = "macos"))]
impl NativeContextMenu {
    pub fn show(&self, _: gpui::Point<gpui::Pixels>, _: &[NativeMenuItem]) -> Option<usize> {
        None
    }
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

/// Whether the user has asked the system to reduce transparency. Read live so
/// a change in System Settings is honored at the next theme application.
#[cfg(target_os = "macos")]
fn reduce_transparency() -> bool {
    use objc2_app_kit::NSWorkspace;
    NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceTransparency()
}

/// Match Cursor's macOS glass window stack without asking GPUI's transparent
/// Metal target to blend two translucent quads. The semantic tint is a native
/// view above active Sidebar vibrancy; GPUI paints clear sidebar chrome and one
/// translucent interaction layer above it.
#[cfg(target_os = "macos")]
pub fn configure_sidebar_material(window: &Window, dark: bool, tint: gpui::Hsla) {
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
        let backdrop = gpui::Rgba::from(tint);
        let background = if dark {
            NSColor::colorWithSRGBRed_green_blue_alpha(
                f64::from(backdrop.r),
                f64::from(backdrop.g),
                f64::from(backdrop.b),
                0.25,
            )
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

        // The palette owns this color. Vibrancy still shows through, and the
        // system's reduce-transparency setting closes that gap entirely rather
        // than leaving the sidebar the one surface a theme cannot reach.
        let tint = gpui::Rgba::from(tint);
        let opacity = if reduce_transparency() { 1.0 } else { 0.92 };
        let tint = NSColor::colorWithSRGBRed_green_blue_alpha(
            f64::from(tint.r),
            f64::from(tint.g),
            f64::from(tint.b),
            opacity,
        );

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
pub fn configure_sidebar_material(_: &Window, _: bool, _: gpui::Hsla) {}

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

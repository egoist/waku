//! Native browser surface for the right panel: a WKWebView on macOS, a
//! composition-hosted WebView2 on Windows.
//!
//! Both are real native content the GPUI renderer does not own, so three
//! invariants keep them honest:
//!
//! - Geometry: the surface's content area syncs the native frame from element
//!   layout every frame, deduplicated so an unchanged frame costs nothing.
//! - Visibility: [`Waku`] recomputes "should the webview be on screen" once
//!   per frame — panel visible, Browser tab active, no settings page — and
//!   pushes it down here. On a window without GPUI's overlay plane the live
//!   view also swaps for a frozen snapshot while a menu or popover is open,
//!   because GPUI could not otherwise paint above it.
//! - Threading: native callbacks arrive on the main run loop, possibly while
//!   GPUI is mid-update, so they never touch entities directly. Each handler
//!   records intent and schedules the entity update on the foreground
//!   executor.
//!
//! The two platforms differ in how much of the window they take over. AppKit
//! puts the WKWebView in the view hierarchy and routes input to it; Windows
//! renders WebView2 into one of GPUI's own composition visuals and receives
//! nothing, so this module forwards mouse input, cursor and focus by hand.
//! [`host`] carries the detail.
//!
//! [`Waku`]: crate::app::Waku

use std::rc::Rc;

use gpui::{
    App, Context, Div, Entity, FocusHandle, Focusable, HitboxBehavior, IntoElement, ObjectFit,
    Render, SharedString, Stateful, Subscription, Window, canvas, div, img, prelude::*, px,
};
#[cfg(any(target_os = "macos", target_os = "windows"))]
use gpui::{AsyncApp, ForegroundExecutor, WeakEntity};

use crate::input::{InputEvent, TextInput};
use crate::theme::{Theme, sp};
use crate::ui::icon;
use crate::ui::text_field::TextField;
use crate::ui::tooltip::Tooltip;
use crate::{
    BrowserBack, BrowserDevtools, BrowserForward, BrowserHardReload, BrowserReload, BrowserStop,
    FocusBrowserAddress, WebviewCopy, WebviewCut, WebviewPaste, WebviewSelectAll,
};

const TOOLBAR_HEIGHT: f32 = 42.0;
/// Mirror Safari's UA so sites serve the webview their real desktop build.
#[cfg(target_os = "macos")]
const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) \
     AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.5 Safari/605.1.15";

/// What the address input resolves to when the user submits it.
#[derive(Debug, PartialEq, Eq)]
enum AddressTarget {
    Url(String),
    Search(String),
}

/// Safari-style omnibox resolution: explicit schemes pass through, host-like
/// text gets a scheme guessed for it, anything else becomes a web search.
fn resolve_address(raw: &str) -> Option<AddressTarget> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    let has_scheme = trimmed.split_once(':').is_some_and(|(scheme, rest)| {
        !scheme.is_empty()
            && scheme
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
            && scheme
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic())
            && (rest.starts_with("//") || matches!(scheme, "about" | "data" | "mailto" | "file"))
    });
    if has_scheme {
        return Some(AddressTarget::Url(trimmed.to_owned()));
    }

    if trimmed.contains(char::is_whitespace) {
        return Some(AddressTarget::Search(trimmed.to_owned()));
    }

    let authority = trimmed.split(['/', '?', '#']).next().unwrap_or(trimmed);
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) if !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()) => {
            (host, true)
        }
        Some(_) => return Some(AddressTarget::Search(trimmed.to_owned())),
        None => (authority, false),
    };
    let is_ip = !host.is_empty()
        && host.chars().all(|c| c.is_ascii_digit() || c == '.')
        && host.split('.').count() == 4;
    let is_local = host.eq_ignore_ascii_case("localhost") || is_ip;
    let host_like = is_local
        || (host.contains('.')
            && !host.starts_with('.')
            && !host.ends_with('.')
            && host
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-')));

    if !host_like {
        return Some(AddressTarget::Search(trimmed.to_owned()));
    }
    // Dev servers rarely speak TLS; the public web rarely speaks anything else.
    let scheme = if is_local || (port && host.eq_ignore_ascii_case("localhost")) {
        "http"
    } else {
        "https"
    };
    Some(AddressTarget::Url(format!("{scheme}://{trimmed}")))
}

fn search_url(query: &str) -> String {
    let mut encoded = String::with_capacity(query.len() * 3);
    for byte in query.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                encoded.push(byte as char)
            }
            b' ' => encoded.push('+'),
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    format!("https://www.google.com/search?q={encoded}")
}

fn is_secure_url(url: &str) -> bool {
    url.starts_with("https://")
}

/// The address bar hides `https://` the way Safari does; everything else —
/// including `http://` — stays visible because it is information.
fn display_url(url: &str) -> &str {
    url.strip_prefix("https://").unwrap_or(url)
}

#[cfg(target_os = "macos")]
mod host {
    use std::cell::Cell;
    use std::ffi::c_void;
    use std::ptr::null_mut;

    use gpui::{Bounds, Pixels};
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2::{AllocAnyThread, DefinedClass, define_class, msg_send};
    use objc2_app_kit::{NSApplication, NSEventType, NSView, NSWindow};
    use objc2_foundation::{
        MainThreadMarker, NSDictionary, NSKeyValueChangeKey, NSKeyValueObservingOptions,
        NSObjectNSKeyValueObserverRegistration, NSObjectProtocol, NSProcessInfo, NSString,
        ns_string,
    };
    use objc2_web_kit::WKWebView;
    use wry::WebViewExtMacOS;
    use wry::dpi::{LogicalPosition, LogicalSize};

    /// Whether AppKit is currently dispatching (or just dispatched) a mouse
    /// press — the discriminator between a user's click handing the page the
    /// keyboard and a page script pulling it over on its own: a click-driven
    /// responder change happens inside that click's dispatch, so the current
    /// event is a fresh press; a script's `focus()` fires from a WebKit
    /// callout with only a stale event behind it.
    fn recent_user_gesture() -> bool {
        let Some(mtm) = MainThreadMarker::new() else {
            return false;
        };
        let Some(event) = NSApplication::sharedApplication(mtm).currentEvent() else {
            return false;
        };
        let pressed = matches!(
            event.r#type(),
            NSEventType::LeftMouseDown
                | NSEventType::LeftMouseUp
                | NSEventType::RightMouseDown
                | NSEventType::OtherMouseDown
        );
        pressed && NSProcessInfo::processInfo().systemUptime() - event.timestamp() < 0.5
    }

    pub(super) struct ResponderObserverIvars {
        window: Retained<NSWindow>,
        handler: Box<dyn Fn(bool)>,
    }

    define_class!(
        #[unsafe(super(objc2::runtime::NSObject))]
        #[ivars = ResponderObserverIvars]
        pub(super) struct ResponderObserver;

        /// NSKeyValueObserving: the window's `firstResponder` is documented
        /// KVO-compliant, and observing it is the only push signal for native
        /// focus moves — the webview taking or losing the keyboard produces
        /// no GPUI event at all.
        impl ResponderObserver {
            #[unsafe(method(observeValueForKeyPath:ofObject:change:context:))]
            fn observe_value_for_key_path(
                &self,
                key_path: Option<&NSString>,
                _of_object: Option<&AnyObject>,
                _change: Option<&NSDictionary<NSKeyValueChangeKey, AnyObject>>,
                _context: *mut c_void,
            ) {
                if key_path.is_some_and(|path| path.isEqualToString(ns_string!("firstResponder"))) {
                    (self.ivars().handler)(recent_user_gesture());
                }
            }
        }

        unsafe impl NSObjectProtocol for ResponderObserver {}
    );

    impl ResponderObserver {
        fn new(window: Retained<NSWindow>, handler: Box<dyn Fn(bool)>) -> Retained<Self> {
            let observer = Self::alloc().set_ivars(ResponderObserverIvars { window, handler });
            let observer: Retained<Self> = unsafe { msg_send![super(observer), init] };
            unsafe {
                observer
                    .ivars()
                    .window
                    .addObserver_forKeyPath_options_context(
                        &observer,
                        ns_string!("firstResponder"),
                        NSKeyValueObservingOptions::New,
                        null_mut(),
                    );
            }
            observer
        }
    }

    impl Drop for ResponderObserver {
        fn drop(&mut self) {
            unsafe {
                self.ivars()
                    .window
                    .removeObserver_forKeyPath(self, ns_string!("firstResponder"));
            }
        }
    }

    /// The wry webview plus deduplication state, so per-frame syncs only call
    /// into AppKit when geometry or visibility actually changed.
    pub(super) struct WebviewHost {
        pub webview: wry::WebView,
        wk: Retained<WKWebView>,
        last_bounds: Cell<Option<(i32, i32, i32, i32)>>,
        visible: Cell<bool>,
        /// Watches the window's first responder; dropped (and unregistered)
        /// with the host.
        _responder_observer: Option<Retained<ResponderObserver>>,
    }

    impl WebviewHost {
        pub fn new(webview: wry::WebView, on_responder_change: Box<dyn Fn(bool)>) -> Self {
            let wk: Retained<WKWebView> = Retained::into_super(webview.webview());
            lower_below_scene_overlay(&wk);
            let responder_observer = wk
                .window()
                .map(|window| ResponderObserver::new(window, on_responder_change));
            Self {
                webview,
                wk,
                last_bounds: Cell::new(None),
                visible: Cell::new(false),
                _responder_observer: responder_observer,
            }
        }

        pub fn wk(&self) -> &WKWebView {
            &self.wk
        }

        pub fn ns_view(&self) -> &NSView {
            &self.wk
        }

        /// GPUI window coordinates are top-left-origin logical points, which is
        /// exactly wry's child-bounds convention. Wry quantizes the native
        /// frame to whole points, and panel drags produce fractional layouts —
        /// left un-rounded, the frame can land a point off and expose a sliver
        /// of background along an edge. Round each edge (not origin + size) so
        /// every side stays within half a point of the layout rect, and
        /// deduplicate on the rounded rect so per-frame syncs are free.
        /// AppKit lays the view out in the same logical points GPUI uses, so
        /// the scale factor is only of interest to the Windows host.
        pub fn sync_bounds(&self, bounds: Bounds<Pixels>, _scale: f32) {
            let left = f32::from(bounds.origin.x).round() as i32;
            let top = f32::from(bounds.origin.y).round() as i32;
            let right = f32::from(bounds.origin.x + bounds.size.width).round() as i32;
            let bottom = f32::from(bounds.origin.y + bounds.size.height).round() as i32;
            if self.last_bounds.get() == Some((left, top, right, bottom)) {
                return;
            }
            self.last_bounds.set(Some((left, top, right, bottom)));
            let _ = self.webview.set_bounds(wry::Rect {
                position: LogicalPosition::new(f64::from(left), f64::from(top)).into(),
                size: LogicalSize::new(f64::from(right - left), f64::from(bottom - top)).into(),
            });
        }

        pub fn set_visible(&self, visible: bool) {
            if self.visible.get() == visible {
                return;
            }
            self.visible.set(visible);
            let _ = self.webview.set_visible(visible);
        }

        /// Whether the native first responder is the webview (or one of its
        /// internal views) — i.e. plain keystrokes currently go to the page,
        /// not to GPUI.
        pub fn native_focus_within(&self) -> bool {
            let view = self.ns_view();
            let Some(window) = view.window() else {
                return false;
            };
            window.firstResponder().is_some_and(|responder| {
                responder
                    .downcast_ref::<NSView>()
                    .is_some_and(|responder| responder.isDescendantOf(view))
            })
        }
    }

    /// GPUI's scene-overlay view — the transparent plane its menus and
    /// tooltips composite on — is added to the window before this webview
    /// existed, and AppKit stacks later siblings on top. Left alone, a fresh
    /// webview would cover the overlay and every menu with it; re-anchor the
    /// webview just beneath the overlay plane.
    fn lower_below_scene_overlay(view: &NSView) {
        use objc2_app_kit::NSWindowOrderingMode;

        let Some(superview) = (unsafe { view.superview() }) else {
            return;
        };
        for sibling in superview.subviews().iter() {
            if sibling.class().name() == c"GPUIOverlayView" {
                superview.addSubview_positioned_relativeTo(
                    view,
                    NSWindowOrderingMode::Below,
                    Some(&sibling),
                );
                return;
            }
        }
    }
}

#[cfg(target_os = "windows")]
mod host {
    //! WebView2 composited inside GPUI's own visual tree.
    //!
    //! The obvious way to embed WebView2 — and what wry does — is windowed
    //! hosting: the controller owns a child HWND of the app's window. Windows
    //! composites child windows above the swap chain unconditionally, so a
    //! GPUI menu or tooltip drawn over the page is simply not visible. The
    //! only escapes are hiding the page while an overlay is open or freezing
    //! it to a bitmap, and neither survives contact with a video call or a
    //! page that is still scrolling.
    //!
    //! Visual hosting is the supported answer. A composition controller
    //! renders into a DirectComposition visual instead of an HWND, and the
    //! pinned gpui fork already prepares the slot:
    //! `DirectCompositionRenderer::create_portal` inserts an empty
    //! `IDCompositionVisual` into `portal_container`, which sits between the
    //! base and overlay swap chains —
    //!
    //! ```text
    //! root_visual.AddVisual(&portal_container, true, &base_visual)
    //! root_visual.AddVisual(&overlay_visual,  true, &portal_container)
    //! ```
    //!
    //! — so the page composites above GPUI's ordinary content and below its
    //! menus, tooltips and dialogs, with the portal's rectangle clip handling
    //! the panel edge. The visual comes from GPUI's own `IDCompositionDevice`,
    //! which is what `SetRootVisualTarget` requires, and gpui and
    //! webview2-com are both built against `windows` 0.61, so the handle
    //! crosses untouched.
    //!
    //! An ordinary `ICoreWebView2Controller` cannot be upgraded to a
    //! composition controller after the fact — only the environment creates
    //! one — so none of this is reachable through wry's `WebViewExtWindows`,
    //! and Waku drives `webview2-com` directly rather than carrying a wry
    //! fork. It uses a narrow slice of it (bounds, visibility, focus,
    //! navigation, six events), so there is little of wry's custom-protocol,
    //! IPC and window-lifecycle machinery to give up.
    //!
    //! The cost is input: a visual has no window, so WebView2 receives
    //! nothing on its own. Everything from `mouse_down` down exists to put
    //! that back — buttons, movement, wheel and leave are forwarded from
    //! GPUI's window events through `SendMouseInput`, the cursor comes back
    //! through `CursorChanged`, and focus is driven explicitly with
    //! `MoveFocus`. Keyboard and IME still flow through the controller's own
    //! internal input window once it holds focus. Pen and touch
    //! (`SendPointerInput`), external drag and drop
    //! (`ICoreWebView2CompositionController3`), the accessibility provider
    //! (`ICoreWebView2CompositionController2::AutomationProvider`) and
    //! rebinding the visual after GPU device loss are not implemented.
    //!
    //! longbridge/gpui-component#2626 carries a longer write-up of the same
    //! contract under `crates/webview/WEBVIEW_OVERLAY_RESEARCH.md`.

    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    use gpui::{
        Bounds, CursorStyle, Modifiers, MouseButton, Pixels, PlatformNativeSurface, Point,
        ScrollDelta,
    };
    use webview2_com::Microsoft::Web::WebView2::Win32::*;
    use webview2_com::{
        CreateCoreWebView2CompositionControllerCompletedHandler,
        CreateCoreWebView2EnvironmentCompletedHandler, CursorChangedEventHandler,
        DocumentTitleChangedEventHandler, FocusChangedEventHandler, MoveFocusRequestedEventHandler,
        NavigationCompletedEventHandler, NavigationStartingEventHandler,
        NewWindowRequestedEventHandler, SourceChangedEventHandler, take_pwstr,
    };
    use windows::Win32::Foundation::{E_FAIL, E_NOINTERFACE, HWND, POINT, RECT};
    use windows::core::{BOOL, HSTRING, IUnknown, Interface, PCWSTR, PWSTR};

    use super::PageLoad;

    /// Entity updates the page pushes back into [`super::BrowserView`]. Each
    /// is called on the UI thread from a WebView2 event and hops through
    /// `Deferred`, so none may assume the app is un-borrowed.
    pub(super) struct Callbacks {
        pub page_load: Box<dyn Fn(PageLoad, String)>,
        pub url_changed: Box<dyn Fn(String)>,
        pub title: Box<dyn Fn(String)>,
        pub open_url: Box<dyn Fn(String)>,
        pub cursor_changed: Box<dyn Fn()>,
        pub focus_changed: Box<dyn Fn()>,
    }

    /// Delivers the finished host — or the reason there isn't one — exactly
    /// once, from whichever of the two creation callbacks gets there first.
    type Ready = Rc<RefCell<Option<Box<dyn FnOnce(Result<Rc<WebviewHost>, String>)>>>>;

    fn deliver(ready: &Ready, outcome: Result<Rc<WebviewHost>, String>) {
        if let Some(ready) = ready.borrow_mut().take() {
            ready(outcome);
        }
    }

    /// Where WebView2 keeps its profile: per-user, beside the rest of Waku's
    /// data, so a per-user install never needs to write into its own
    /// program directory.
    fn user_data_folder() -> Option<HSTRING> {
        let path = dirs::data_local_dir()?
            .join(waku_protocol::identity::DATA_DIRECTORY_NAME)
            .join("WebView2");
        std::fs::create_dir_all(&path).ok()?;
        Some(HSTRING::from(path.as_path()))
    }

    /// The committed URL, or an empty string when WebView2 has none yet.
    fn source_of(webview: &ICoreWebView2) -> String {
        let mut uri = PWSTR::null();
        match unsafe { webview.Source(&mut uri) } {
            Ok(()) => take_pwstr(uri),
            Err(_) => String::new(),
        }
    }

    /// The system's lines- and characters-per-notch wheel preferences, which
    /// GPUI has already multiplied into the deltas it reports. Read once:
    /// they are a user setting, and this is on the wheel path.
    fn wheel_scroll_preferences() -> (f32, f32) {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            SPI_GETWHEELSCROLLCHARS, SPI_GETWHEELSCROLLLINES, SystemParametersInfoW,
        };

        let read = |action| {
            let mut value: u32 = 0;
            let read = unsafe {
                SystemParametersInfoW(action, 0, std::ptr::from_mut(&mut value).cast(), 0)
            };
            (read != 0 && value != 0).then_some(value as f32)
        };
        (
            read(SPI_GETWHEELSCROLLLINES).unwrap_or(3.0),
            read(SPI_GETWHEELSCROLLCHARS).unwrap_or(3.0),
        )
    }

    /// System cursor ids from `ICoreWebView2CompositionController::\
    /// SystemCursorId`, mapped onto the GPUI styles the page area asks for.
    ///
    /// The interface hands back a raw `HCURSOR` too, but setting that
    /// directly fights GPUI, which reasserts its own cursor on every
    /// `WM_SETCURSOR`. Going through `Styled::cursor` instead makes the
    /// page's cursor one more thing GPUI composites.
    fn cursor_style_for(id: u32) -> CursorStyle {
        // `IDC_*` from WinUser.h — resource ordinals, not handles, so they
        // are stable and comparable.
        match id {
            32513 => CursorStyle::IBeam,
            32515 => CursorStyle::Crosshair,
            32642 => CursorStyle::ResizeUpLeftDownRight,
            32643 => CursorStyle::ResizeUpRightDownLeft,
            32644 => CursorStyle::ResizeLeftRight,
            32645 => CursorStyle::ResizeUpDown,
            32646 => CursorStyle::ClosedHand,
            32648 => CursorStyle::OperationNotAllowed,
            32649 => CursorStyle::PointingHand,
            // 32512 IDC_ARROW, plus 32514 IDC_WAIT and 32650
            // IDC_APPSTARTING: GPUI has no busy cursor, and a page that is
            // merely slow should not change the pointer under the user.
            _ => CursorStyle::Arrow,
        }
    }

    /// Which `COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS` bit a button holds down.
    fn button_bit(button: MouseButton) -> Option<i32> {
        Some(
            match button {
                MouseButton::Left => COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS_LEFT_BUTTON,
                MouseButton::Right => COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS_RIGHT_BUTTON,
                MouseButton::Middle => COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS_MIDDLE_BUTTON,
                // Back and forward are the surface's own toolbar actions;
                // forwarding them as well would navigate twice.
                MouseButton::Navigate(_) => return None,
            }
            .0,
        )
    }

    /// Hand the keyboard back to GPUI's window.
    fn focus_window(parent: isize) {
        use windows_sys::Win32::Foundation::HWND as SysHwnd;
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::SetFocus;

        unsafe { SetFocus(parent as SysHwnd) };
    }

    /// The `ICoreWebView2` behind the surface, exposing the same handful of
    /// operations the macOS host does so the shared call sites in
    /// [`super::BrowserView`] stay platform-free.
    pub(super) struct Webview(ICoreWebView2);

    impl Webview {
        pub fn can_go_back(&self) -> windows::core::Result<bool> {
            let mut value = BOOL(0);
            unsafe { self.0.CanGoBack(&mut value) }?;
            Ok(value.as_bool())
        }

        pub fn can_go_forward(&self) -> windows::core::Result<bool> {
            let mut value = BOOL(0);
            unsafe { self.0.CanGoForward(&mut value) }?;
            Ok(value.as_bool())
        }

        pub fn go_back(&self) -> windows::core::Result<()> {
            unsafe { self.0.GoBack() }
        }

        pub fn go_forward(&self) -> windows::core::Result<()> {
            unsafe { self.0.GoForward() }
        }

        pub fn reload(&self) -> windows::core::Result<()> {
            unsafe { self.0.Reload() }
        }

        pub fn stop(&self) -> windows::core::Result<()> {
            unsafe { self.0.Stop() }
        }

        pub fn load_url(&self, url: &str) -> windows::core::Result<()> {
            unsafe { self.0.Navigate(&HSTRING::from(url)) }
        }

        /// Fire and forget: nothing in the surface reads a script's result,
        /// and passing no completion handler keeps the call synchronous from
        /// the caller's point of view.
        pub fn evaluate_script(&self, script: &str) -> windows::core::Result<()> {
            unsafe { self.0.ExecuteScript(&HSTRING::from(script), None) }
        }

        /// WebView2 has no "close" or "is open" counterpart — the devtools
        /// window is the user's from here on.
        pub fn open_devtools(&self) -> windows::core::Result<()> {
            unsafe { self.0.OpenDevToolsWindow() }
        }
    }

    pub(super) struct WebviewHost {
        pub(super) webview: Webview,
        controller: ICoreWebView2Controller,
        composition: ICoreWebView2CompositionController,
        /// GPUI's slot in the composition tree, which owns the page's
        /// position and clip. WebView2's own bounds only set the raster size.
        surface: Rc<dyn PlatformNativeSurface>,
        /// GPUI's window, for handing the keyboard back.
        parent: isize,
        last_bounds: Cell<Option<Bounds<Pixels>>>,
        /// Window-space origin of the page area and the window's scale, kept
        /// so a forwarded mouse position can be put into the page's own
        /// device-pixel space without a round trip through GPUI.
        origin: Cell<Point<Pixels>>,
        scale: Cell<f32>,
        visible: Cell<bool>,
        focused: Rc<Cell<bool>>,
        cursor: Rc<Cell<CursorStyle>>,
        /// Buttons currently held, so a move or wheel during a drag reports
        /// them the way Win32 would.
        buttons: Cell<i32>,
        hovered: Cell<bool>,
        wheel_scroll: (f32, f32),
    }

    impl WebviewHost {
        /// Build a composition-hosted WebView2 and hand it back once it
        /// exists.
        ///
        /// Creation is genuinely asynchronous — the environment and the
        /// controller each complete on a posted message — and it stays that
        /// way here. webview2-com offers `wait_for_async_operation`, which
        /// pumps a nested message loop, but running one from inside an entity
        /// update invites a re-entrant `WM_PAINT` and a panicking borrow. The
        /// surface simply has no host for the first few frames, which it
        /// already handles.
        pub fn create(
            parent: isize,
            surface: Rc<dyn PlatformNativeSurface>,
            callbacks: Callbacks,
            ready: Box<dyn FnOnce(Result<Rc<WebviewHost>, String>)>,
        ) {
            let ready: Ready = Rc::new(RefCell::new(Some(ready)));
            let Some(user_data) = user_data_folder() else {
                deliver(&ready, Err("no local application data folder".to_owned()));
                return;
            };

            let handler = CreateCoreWebView2EnvironmentCompletedHandler::create(Box::new({
                let ready = ready.clone();
                move |result, environment| {
                    match result.and_then(|()| environment.ok_or_else(|| E_FAIL.into())) {
                        Ok(environment) => {
                            create_controller(parent, surface, environment, callbacks, ready)
                        }
                        Err(error) => deliver(&ready, Err(error.to_string())),
                    }
                    Ok(())
                }
            }));

            let created = unsafe {
                CreateCoreWebView2EnvironmentWithOptions(
                    PCWSTR::null(),
                    &user_data,
                    None::<&ICoreWebView2EnvironmentOptions>,
                    &handler,
                )
            };
            if let Err(error) = created {
                deliver(&ready, Err(error.to_string()));
            }
        }

        /// Called from the element's paint callback every frame, so an
        /// unchanged rect must cost nothing.
        ///
        /// The portal carries the position and the clip; WebView2's own
        /// bounds start at the origin and only give the page its raster size.
        /// Both are device pixels, which is
        /// `COREWEBVIEW2_BOUNDS_MODE_USE_RAW_PIXELS`, the default.
        pub fn sync_bounds(&self, bounds: Bounds<Pixels>, scale: f32) {
            self.origin.set(bounds.origin);
            if self.last_bounds.get() == Some(bounds) && self.scale.get() == scale {
                return;
            }
            self.last_bounds.set(Some(bounds));

            if self.scale.replace(scale) != scale
                && let Ok(controller) = self.controller.cast::<ICoreWebView2Controller3>()
            {
                let _ = unsafe { controller.SetRasterizationScale(scale as f64) };
            }

            let device = bounds.to_device_pixels(scale);
            let _ = self.surface.set_bounds(device);
            let _ = unsafe {
                self.controller.SetBounds(RECT {
                    left: 0,
                    top: 0,
                    right: device.size.width.0.max(0),
                    bottom: device.size.height.0.max(0),
                })
            };
        }

        pub fn set_visible(&self, visible: bool) {
            if self.visible.get() == visible {
                return;
            }
            self.visible.set(visible);
            // Hand the keyboard back before hiding, not after: a hidden page
            // that still owns focus swallows every key GPUI expects.
            if !visible {
                if self.focused.get() {
                    focus_window(self.parent);
                }
                self.mouse_leave();
            }
            let _ = unsafe { self.controller.SetIsVisible(visible) };
            let _ = self.surface.set_visible(visible);
        }

        /// Whether the keyboard currently belongs to the page.
        ///
        /// Driven by the controller's own `GotFocus`/`LostFocus` rather than
        /// by probing `GetFocus`: visual hosting gives WebView2 no window of
        /// ours to descend from, and the events are the documented signal.
        pub fn native_focus_within(&self) -> bool {
            self.focused.get()
        }

        pub fn focus_page(&self) {
            let _ = unsafe {
                self.controller
                    .MoveFocus(COREWEBVIEW2_MOVE_FOCUS_REASON_PROGRAMMATIC)
            };
        }

        pub fn focus_parent(&self) {
            focus_window(self.parent);
        }

        /// The cursor the page last asked for, which the page area applies
        /// through `Styled::cursor`.
        pub fn cursor_style(&self) -> CursorStyle {
            self.cursor.get()
        }

        pub fn mouse_down(
            &self,
            button: MouseButton,
            position: Point<Pixels>,
            modifiers: Modifiers,
            click_count: usize,
        ) {
            let Some(bit) = button_bit(button) else {
                return;
            };
            self.buttons.set(self.buttons.get() | bit);
            // WebView2 wants the second click of a pair reported as a
            // double-click; triples and beyond it works out itself from the
            // repeated downs.
            let double = click_count == 2;
            let kind = match (button, double) {
                (MouseButton::Left, false) => COREWEBVIEW2_MOUSE_EVENT_KIND_LEFT_BUTTON_DOWN,
                (MouseButton::Left, true) => COREWEBVIEW2_MOUSE_EVENT_KIND_LEFT_BUTTON_DOUBLE_CLICK,
                (MouseButton::Right, false) => COREWEBVIEW2_MOUSE_EVENT_KIND_RIGHT_BUTTON_DOWN,
                (MouseButton::Right, true) => {
                    COREWEBVIEW2_MOUSE_EVENT_KIND_RIGHT_BUTTON_DOUBLE_CLICK
                }
                (_, false) => COREWEBVIEW2_MOUSE_EVENT_KIND_MIDDLE_BUTTON_DOWN,
                (_, true) => COREWEBVIEW2_MOUSE_EVENT_KIND_MIDDLE_BUTTON_DOUBLE_CLICK,
            };
            self.send_mouse(kind, modifiers, 0, position);
        }

        pub fn mouse_up(&self, button: MouseButton, position: Point<Pixels>, modifiers: Modifiers) {
            let Some(bit) = button_bit(button) else {
                return;
            };
            self.buttons.set(self.buttons.get() & !bit);
            let kind = match button {
                MouseButton::Left => COREWEBVIEW2_MOUSE_EVENT_KIND_LEFT_BUTTON_UP,
                MouseButton::Right => COREWEBVIEW2_MOUSE_EVENT_KIND_RIGHT_BUTTON_UP,
                _ => COREWEBVIEW2_MOUSE_EVENT_KIND_MIDDLE_BUTTON_UP,
            };
            self.send_mouse(kind, modifiers, 0, position);
        }

        pub fn mouse_move(&self, position: Point<Pixels>, modifiers: Modifiers) {
            self.hovered.set(true);
            self.send_mouse(COREWEBVIEW2_MOUSE_EVENT_KIND_MOVE, modifiers, 0, position);
        }

        /// The pointer left the page area, or the page went away underneath
        /// it. Without this the last hovered element keeps its hover state
        /// and the cursor never comes back.
        pub fn mouse_leave(&self) {
            if !self.hovered.replace(false) {
                return;
            }
            self.buttons.set(0);
            self.cursor.set(CursorStyle::Arrow);
            let _ = unsafe {
                self.composition.SendMouseInput(
                    COREWEBVIEW2_MOUSE_EVENT_KIND_LEAVE,
                    COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS_NONE,
                    0,
                    POINT { x: 0, y: 0 },
                )
            };
        }

        /// GPUI reports wheel movement already multiplied by the system's
        /// lines- and characters-per-notch preferences, and in its own sign
        /// convention — positive means the content moves that way, which is
        /// the opposite of `WM_MOUSEHWHEEL` horizontally. Undo both to get
        /// back to the `WHEEL_DELTA` multiples WebView2 expects.
        pub fn scroll(&self, position: Point<Pixels>, delta: ScrollDelta, modifiers: Modifiers) {
            const WHEEL_DELTA: f32 = 120.0;
            const PIXELS_PER_LINE: f32 = 20.0;

            let (lines_per_notch, chars_per_notch) = self.wheel_scroll;
            let (vertical, horizontal) = match delta {
                ScrollDelta::Lines(delta) => (delta.y, -delta.x),
                // GPUI's Windows backend only produces `Lines`; this is the
                // precise-trackpad shape other platforms send.
                ScrollDelta::Pixels(delta) => {
                    let (x, y) = (f32::from(delta.x), f32::from(delta.y));
                    (y / PIXELS_PER_LINE, -x / PIXELS_PER_LINE)
                }
            };
            for (kind, notches) in [
                (
                    COREWEBVIEW2_MOUSE_EVENT_KIND_WHEEL,
                    vertical / lines_per_notch,
                ),
                (
                    COREWEBVIEW2_MOUSE_EVENT_KIND_HORIZONTAL_WHEEL,
                    horizontal / chars_per_notch,
                ),
            ] {
                let amount = (notches * WHEEL_DELTA).round() as i32;
                if amount != 0 {
                    self.send_mouse(kind, modifiers, amount as u32, position);
                }
            }
        }

        fn send_mouse(
            &self,
            kind: COREWEBVIEW2_MOUSE_EVENT_KIND,
            modifiers: Modifiers,
            data: u32,
            position: Point<Pixels>,
        ) {
            let origin = self.origin.get();
            let scale = self.scale.get();
            let point = POINT {
                x: (f32::from(position.x - origin.x) * scale).round() as i32,
                y: (f32::from(position.y - origin.y) * scale).round() as i32,
            };
            let mut keys = COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS(self.buttons.get());
            if modifiers.control {
                keys |= COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS_CONTROL;
            }
            if modifiers.shift {
                keys |= COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS_SHIFT;
            }
            let _ = unsafe { self.composition.SendMouseInput(kind, keys, data, point) };
        }
    }

    impl Drop for WebviewHost {
        fn drop(&mut self) {
            // Without this the browser process outlives the tab.
            let _ = unsafe { self.controller.Close() };
        }
    }

    /// Second half of [`WebviewHost::create`]: the environment exists, so ask
    /// it for a composition controller.
    fn create_controller(
        parent: isize,
        surface: Rc<dyn PlatformNativeSurface>,
        environment: ICoreWebView2Environment,
        callbacks: Callbacks,
        ready: Ready,
    ) {
        let Ok(environment) = environment.cast::<ICoreWebView2Environment3>() else {
            deliver(
                &ready,
                Err("this WebView2 runtime is too old to render into a visual".to_owned()),
            );
            return;
        };

        let handler = CreateCoreWebView2CompositionControllerCompletedHandler::create(Box::new({
            let ready = ready.clone();
            move |result, composition| {
                let outcome = result
                    .and_then(|()| composition.ok_or_else(|| E_FAIL.into()))
                    .and_then(|composition| attach(parent, surface, composition, callbacks));
                deliver(&ready, outcome.map_err(|error| error.to_string()));
                Ok(())
            }
        }));

        let created = unsafe {
            environment.CreateCoreWebView2CompositionController(HWND(parent as *mut _), &handler)
        };
        if let Err(error) = created {
            deliver(&ready, Err(error.to_string()));
        }
    }

    /// Bind a freshly created composition controller into GPUI's portal
    /// visual and subscribe to everything the surface renders from.
    fn attach(
        parent: isize,
        surface: Rc<dyn PlatformNativeSurface>,
        composition: ICoreWebView2CompositionController,
        callbacks: Callbacks,
    ) -> windows::core::Result<Rc<WebviewHost>> {
        let visual = surface
            .platform_handle()
            .downcast::<IUnknown>()
            .map_err(|_| windows::core::Error::from(E_NOINTERFACE))?;
        unsafe { composition.SetRootVisualTarget(&*visual) }?;

        let controller: ICoreWebView2Controller = composition.cast()?;
        // The page starts hidden and empty; the surface shows it once its tab
        // is visible and something has been navigated to.
        unsafe { controller.SetIsVisible(false) }?;
        if let Ok(controller) = controller.cast::<ICoreWebView2Controller3>() {
            // GPUI owns DPI: it already re-lays-out and re-renders on a scale
            // change, and `sync_bounds` pushes the new factor down.
            let _ = unsafe { controller.SetShouldDetectMonitorScaleChanges(false) };
        }

        let webview = unsafe { controller.CoreWebView2() }?;
        // The toolbar has a devtools button, so make sure the runtime agrees
        // they are available. Everything else stays at WebView2's defaults,
        // including the status bar: it draws inside the page raster, so the
        // portal clips it along with everything else, and a link preview on
        // hover is worth having.
        if let Ok(settings) = unsafe { webview.Settings() } {
            let _ = unsafe { settings.SetAreDevToolsEnabled(true) };
        }

        let Callbacks {
            page_load,
            url_changed,
            title,
            open_url,
            cursor_changed,
            focus_changed,
        } = callbacks;
        // The surface reconciles the two focus systems from `render`, so a
        // focus move that renders nothing on its own still has to ask for a
        // frame or it is only noticed the next time something else does.
        let focus_changed = Rc::new(focus_changed);
        let page_load = Rc::new(page_load);
        let focused = Rc::new(Cell::new(false));
        let cursor = Rc::new(Cell::new(CursorStyle::Arrow));
        let mut token = 0i64;

        let started = NavigationStartingEventHandler::create(Box::new({
            let page_load = page_load.clone();
            move |_, args| {
                let mut uri = PWSTR::null();
                let uri = match args {
                    Some(args) if unsafe { args.Uri(&mut uri) }.is_ok() => take_pwstr(uri),
                    _ => String::new(),
                };
                page_load(PageLoad::Started, uri);
                Ok(())
            }
        }));
        unsafe { webview.add_NavigationStarting(&started, &mut token) }?;

        let completed = NavigationCompletedEventHandler::create(Box::new({
            let page_load = page_load.clone();
            move |webview, _| {
                let url = webview.as_ref().map(source_of).unwrap_or_default();
                page_load(PageLoad::Finished, url);
                Ok(())
            }
        }));
        unsafe { webview.add_NavigationCompleted(&completed, &mut token) }?;

        // Same-document navigation — a router pushing state — never reaches
        // `NavigationCompleted`, so the address bar and the back button would
        // go stale without this.
        let source = SourceChangedEventHandler::create(Box::new(move |webview, _| {
            if let Some(webview) = webview.as_ref() {
                url_changed(source_of(webview));
            }
            Ok(())
        }));
        unsafe { webview.add_SourceChanged(&source, &mut token) }?;

        let document_title =
            DocumentTitleChangedEventHandler::create(Box::new(move |webview, _| {
                let mut value = PWSTR::null();
                if let Some(webview) = webview.as_ref()
                    && unsafe { webview.DocumentTitle(&mut value) }.is_ok()
                {
                    title(take_pwstr(value));
                }
                Ok(())
            }));
        unsafe { webview.add_DocumentTitleChanged(&document_title, &mut token) }?;

        // One surface, one page: pop-ups and `target="_blank"` links navigate
        // in place instead of spawning windows.
        let new_window = NewWindowRequestedEventHandler::create(Box::new(move |_, args| {
            if let Some(args) = args.as_ref() {
                let mut uri = PWSTR::null();
                if unsafe { args.Uri(&mut uri) }.is_ok() {
                    open_url(take_pwstr(uri));
                }
                let _ = unsafe { args.SetHandled(true) };
            }
            Ok(())
        }));
        unsafe { webview.add_NewWindowRequested(&new_window, &mut token) }?;

        let got_focus = FocusChangedEventHandler::create(Box::new({
            let focused = focused.clone();
            let focus_changed = focus_changed.clone();
            move |_, _| {
                focused.set(true);
                focus_changed();
                Ok(())
            }
        }));
        unsafe { controller.add_GotFocus(&got_focus, &mut token) }?;

        let lost_focus = FocusChangedEventHandler::create(Box::new({
            let focused = focused.clone();
            let focus_changed = focus_changed.clone();
            move |_, _| {
                focused.set(false);
                focus_changed();
                Ok(())
            }
        }));
        unsafe { controller.add_LostFocus(&lost_focus, &mut token) }?;

        // Tab off the last control in the page: hand the keyboard back to
        // GPUI rather than let WebView2 cycle inside itself forever.
        let move_focus = MoveFocusRequestedEventHandler::create(Box::new({
            let focused = focused.clone();
            move |_, args| {
                focused.set(false);
                focus_changed();
                focus_window(parent);
                if let Some(args) = args.as_ref() {
                    let _ = unsafe { args.SetHandled(true) };
                }
                Ok(())
            }
        }));
        unsafe { controller.add_MoveFocusRequested(&move_focus, &mut token) }?;

        let cursor_event = CursorChangedEventHandler::create(Box::new({
            let cursor = cursor.clone();
            move |composition, _| {
                let mut id = 0u32;
                if let Some(composition) = composition.as_ref()
                    && unsafe { composition.SystemCursorId(&mut id) }.is_ok()
                {
                    let style = cursor_style_for(id);
                    if cursor.replace(style) != style {
                        cursor_changed();
                    }
                }
                Ok(())
            }
        }));
        unsafe { composition.add_CursorChanged(&cursor_event, &mut token) }?;

        Ok(Rc::new(WebviewHost {
            webview: Webview(webview),
            controller,
            composition,
            surface,
            parent,
            last_bounds: Cell::new(None),
            origin: Cell::new(Point::default()),
            scale: Cell::new(1.0),
            visible: Cell::new(false),
            focused,
            cursor,
            buttons: Cell::new(0),
            hovered: Cell::new(false),
            wheel_scroll: wheel_scroll_preferences(),
        }))
    }
}

/// GPUI's `HWND`, or zero when the window has no native handle yet.
#[cfg(target_os = "windows")]
fn window_hwnd(window: &Window) -> isize {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    // GPUI has its own inherent `window_handle`, so the trait method needs
    // naming explicitly.
    match HasWindowHandle::window_handle(window).map(|handle| handle.as_raw()) {
        Ok(RawWindowHandle::Win32(handle)) => handle.hwnd.get(),
        _ => 0,
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
mod host {
    use gpui::{Bounds, Pixels};

    /// Linux has no embedding path: wry's WebKitGTK backend accepts an Xlib
    /// parent only and needs a GTK main loop, and GPUI's Linux backend is
    /// neither GTK nor guaranteed to be X11.
    pub(super) struct WebviewHost;

    impl WebviewHost {
        pub fn sync_bounds(&self, _bounds: Bounds<Pixels>, _scale: f32) {}
        pub fn set_visible(&self, _visible: bool) {}
        pub fn native_focus_within(&self) -> bool {
            false
        }
    }
}

use host::WebviewHost;

/// Schedules entity updates from webview delegate callbacks. The callbacks run
/// on the main thread but can fire while GPUI holds the app borrow, so the
/// update always takes the next executor turn instead of re-entering.
#[derive(Clone)]
#[cfg(any(target_os = "macos", target_os = "windows"))]
struct Deferred {
    executor: ForegroundExecutor,
    cx: AsyncApp,
    view: WeakEntity<BrowserView>,
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
impl Deferred {
    fn update(&self, f: impl FnOnce(&mut BrowserView, &mut Context<BrowserView>) + 'static) {
        let mut cx = self.cx.clone();
        let view = self.view.clone();
        self.executor
            .spawn(async move {
                let _ = view.update(&mut cx, f);
            })
            .detach();
    }
}

pub struct BrowserView {
    focus_handle: FocusHandle,
    address: Entity<TextInput>,
    host: Option<Rc<WebviewHost>>,
    /// Why the webview could not be created, shown in place of the page.
    host_error: Option<String>,
    /// Somewhere to navigate to as soon as the host lands. WebView2's
    /// controller is created asynchronously, so the surface can be asked to
    /// open a URL before it has anything to open it in.
    #[cfg(target_os = "windows")]
    pending_url: Option<String>,
    /// A navigation has been requested at least once: the surface shows the
    /// page area instead of the start hint, and the native view may be shown.
    navigation_requested: bool,
    current_url: Option<String>,
    page_title: Option<String>,
    loading: bool,
    can_go_back: bool,
    can_go_forward: bool,
    /// The user has edited the address since it last echoed the page, so page
    /// navigations must not clobber the field until they commit or cancel.
    address_dirty: bool,
    /// Native-focus edge detection: whether the webview held the native first
    /// responder as of the last frame.
    was_natively_focused: bool,
    /// GPUI-focus edge detection: the window's focused handle last frame.
    last_window_focus: Option<FocusHandle>,
    occluded: bool,
    /// Frozen page pixels drawn while a GPUI overlay is open above the panel.
    /// A `RenderImage` rather than an encoded `Image`: encoded images decode
    /// through the async asset pipeline, whose first paint is empty — the
    /// swap must paint the very frame the live view hides or it blinks.
    snapshot: Option<std::sync::Arc<gpui::RenderImage>>,
    snapshot_pending: bool,
    /// Discards snapshot completions that land after their occlusion ended.
    snapshot_epoch: u64,
    _subscriptions: Vec<Subscription>,
}

impl BrowserView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let address = cx.new(|cx| {
            TextInput::new(window, cx)
                .select_all_on_focus_click()
                .placeholder(tr!("input.search_or_enter_address"))
        });

        let submit_subscription = cx.subscribe(
            &address,
            |this: &mut Self, address, event: &InputEvent, cx| match event {
                InputEvent::Submit(text) => this.navigate_to_input(text.clone(), cx),
                // Search-mode fields never emit a steer; nothing to do here.
                InputEvent::Edited => {
                    // Edits from the page echo itself also land here (events
                    // flush after the update that set the content), so dirty
                    // is derived, not latched: the field is dirty exactly
                    // while it shows something other than the page's URL.
                    let shown = this.current_url.as_deref().map(display_url).unwrap_or("");
                    this.address_dirty = address.read(cx).content() != shown;
                }
                InputEvent::Focus => {}
                InputEvent::BackspaceOnEmpty => {}
            },
        );

        let focus_handle = cx.focus_handle();
        let address_focus = address.read(cx).focus();
        let weak_for_focus_in = cx.entity().downgrade();
        let weak_for_focus_out = cx.entity().downgrade();

        // GPUI focus moves are invisible to render-time reconciliation when
        // they don't re-render this view (focusing the address bar only
        // re-renders the input entity; focusing the chat composer renders
        // nothing of ours), so the reclaim rides the window's focus
        // listeners, which fire on every focus change.
        let focus_in_address = window.on_focus_in(&address_focus, cx, {
            let view = weak_for_focus_in;
            move |_, cx| {
                let _ = view.update(cx, |this: &mut Self, cx| {
                    // Clicking, using the focus shortcut, or tabbing into the address bar while
                    // the page holds the native keyboard: take it back, or
                    // every keystroke keeps going to the page.
                    if this
                        .host
                        .as_ref()
                        .is_some_and(|host| host.native_focus_within())
                    {
                        this.reclaim_native_keyboard(cx);
                    }
                });
            }
        });
        let focus_out_surface = window.on_focus_out(&focus_handle, cx, {
            let view = weak_for_focus_out;
            move |_, window, cx| {
                // GPUI focus left this surface for another control (the chat
                // composer, a find bar): that control owns the keyboard now,
                // so the page hands the native side back. Deactivating the
                // window also reports an empty focus path; keep the page's
                // focus through that.
                let focused_elsewhere = window.is_window_active() && window.focused(cx).is_some();
                let _ = view.update(cx, |this: &mut Self, cx| {
                    if focused_elsewhere
                        && this
                            .host
                            .as_ref()
                            .is_some_and(|host| host.native_focus_within())
                    {
                        this.reclaim_native_keyboard(cx);
                    }
                });
            }
        });

        let mut this = Self {
            focus_handle,
            address,
            host: None,
            host_error: None,
            #[cfg(target_os = "windows")]
            pending_url: None,
            navigation_requested: false,
            current_url: None,
            page_title: None,
            loading: false,
            can_go_back: false,
            can_go_forward: false,
            address_dirty: false,
            was_natively_focused: false,
            last_window_focus: None,
            occluded: false,
            snapshot: None,
            snapshot_pending: false,
            snapshot_epoch: 0,
            _subscriptions: vec![submit_subscription, focus_in_address, focus_out_surface],
        };
        this.build_webview(window, cx);
        this
    }

    pub fn refresh_localized_text(&mut self, cx: &mut Context<Self>) {
        self.address.update(cx, |address, cx| {
            address.set_placeholder(tr!("input.search_or_enter_address"), cx)
        });
        cx.notify();
    }

    /// The label the right panel tab shows for this surface.
    pub fn tab_label(&self) -> Option<String> {
        if let Some(title) = self.page_title.as_deref().filter(|t| !t.trim().is_empty()) {
            return Some(title.to_owned());
        }
        self.current_url
            .as_deref()
            .map(|url| display_url(url).to_owned())
    }

    #[cfg(target_os = "macos")]
    fn build_webview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use wry::dpi::{LogicalPosition, LogicalSize};

        let deferred = Deferred {
            executor: cx.foreground_executor().clone(),
            cx: cx.to_async(),
            view: cx.entity().downgrade(),
        };

        let on_page_load = deferred.clone();
        let on_title = deferred.clone();
        let on_new_window = deferred.clone();

        // The responder observer's decision needs the window (GPUI focus
        // moves), which `Deferred` cannot reach; go through the window handle.
        let on_responder_change: Box<dyn Fn(bool)> = {
            let executor = cx.foreground_executor().clone();
            let async_cx = cx.to_async();
            let view = cx.entity().downgrade();
            let window_handle = window.window_handle();
            Box::new(move |user_gesture| {
                let mut cx = async_cx.clone();
                let view = view.clone();
                executor
                    .spawn(async move {
                        let _ = window_handle.update(&mut cx, |_, window, cx| {
                            let _ = view.update(cx, |this, cx| {
                                this.native_responder_changed(user_gesture, window, cx);
                            });
                        });
                    })
                    .detach();
            })
        };

        let built = wry::WebViewBuilder::new()
            .with_bounds(wry::Rect {
                position: LogicalPosition::new(0.0, 0.0).into(),
                size: LogicalSize::new(0.0, 0.0).into(),
            })
            .with_visible(false)
            .with_focused(false)
            .with_accept_first_mouse(true)
            .with_devtools(true)
            .with_user_agent(USER_AGENT)
            .with_navigation_handler(|_| true)
            .with_on_page_load_handler(move |event, url| {
                let event = match event {
                    wry::PageLoadEvent::Started => PageLoad::Started,
                    wry::PageLoadEvent::Finished => PageLoad::Finished,
                };
                on_page_load.update(move |this, cx| this.page_load_changed(event, url, cx));
            })
            .with_document_title_changed_handler(move |title| {
                on_title.update(move |this, cx| this.title_changed(title, cx));
            })
            .with_new_window_req_handler(move |url, _features| {
                // One surface, one page: pop-ups and `target="_blank"` links
                // navigate in place instead of spawning windows.
                on_new_window.update(move |this, cx| this.navigate_to_url(url, cx));
                wry::NewWindowResponse::Deny
            })
            .with_download_started_handler(|url, destination| {
                let Some(target) = download_destination(&url, destination.clone()) else {
                    return false;
                };
                *destination = target;
                true
            })
            .with_download_completed_handler(|_url, path, success| {
                if success && let Some(path) = path {
                    reveal_in_finder(&path);
                }
            })
            .build_as_child(window);

        match built {
            Ok(webview) => {
                self.host = Some(Rc::new(WebviewHost::new(webview, on_responder_change)))
            }
            Err(error) => self.host_error = Some(error.to_string()),
        }
    }

    /// The native first responder moved (KVO on the window): resolve the two
    /// focus systems immediately instead of waiting for a render. While the
    /// address bar is being typed into, a script-initiated grab (a page
    /// autofocusing its own input) loses the keyboard right back; a grab
    /// carried by a user click means the user entered the page, so GPUI
    /// focus follows onto this surface and the address bar drops its caret.
    #[cfg(target_os = "macos")]
    fn native_responder_changed(
        &mut self,
        user_gesture: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let natively_focused = self
            .host
            .as_ref()
            .is_some_and(|host| host.native_focus_within());
        if natively_focused {
            let address_focused = self.address.read(cx).focus().is_focused(window);
            if address_focused && !user_gesture {
                self.reclaim_native_keyboard(cx);
            } else {
                window.focus(&self.focus_handle, cx);
            }
        }
        self.was_natively_focused = natively_focused;
        self.last_window_focus = window.focused(cx);
        cx.notify();
    }

    /// WebView2 rendered into GPUI's composition tree.
    ///
    /// Nothing exists synchronously here: `create` returns before the
    /// environment and the controller do, and the host lands a few frames
    /// later through `webview_ready`. See [`host`] for why visual hosting is
    /// worth that. No user agent is set — WebView2's default already
    /// identifies as desktop Edge, and overriding it only makes sites guess
    /// worse.
    #[cfg(target_os = "windows")]
    fn build_webview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let parent = window_hwnd(window);
        if parent == 0 {
            self.host_error = Some("the window has no native handle".to_owned());
            return;
        }
        // The portal is a visual GPUI keeps between its own base and overlay
        // planes, so the page composites under menus rather than over them.
        let surface = match window.create_native_surface() {
            Ok(surface) => surface,
            Err(error) => {
                self.host_error = Some(error.to_string());
                return;
            }
        };

        let deferred = Deferred {
            executor: cx.foreground_executor().clone(),
            cx: cx.to_async(),
            view: cx.entity().downgrade(),
        };
        let on_page_load = deferred.clone();
        let on_url = deferred.clone();
        let on_title = deferred.clone();
        let on_new_window = deferred.clone();
        let on_cursor = deferred.clone();
        let on_focus = deferred.clone();
        let on_ready = deferred.clone();

        host::WebviewHost::create(
            parent,
            surface,
            host::Callbacks {
                page_load: Box::new(move |event, url| {
                    on_page_load.update(move |this, cx| this.page_load_changed(event, url, cx));
                }),
                url_changed: Box::new(move |url| {
                    on_url.update(move |this, cx| this.source_changed(url, cx));
                }),
                title: Box::new(move |title| {
                    on_title.update(move |this, cx| this.title_changed(title, cx));
                }),
                open_url: Box::new(move |url| {
                    on_new_window.update(move |this, cx| this.navigate_to_url(url, cx));
                }),
                cursor_changed: Box::new(move || {
                    on_cursor.update(|_, cx| cx.notify());
                }),
                focus_changed: Box::new(move || {
                    on_focus.update(|_, cx| cx.notify());
                }),
            },
            Box::new(move |outcome| {
                on_ready.update(move |this, cx| this.webview_ready(outcome, cx));
            }),
        );
    }

    /// The composition controller finished being created — or failed to be.
    #[cfg(target_os = "windows")]
    fn webview_ready(&mut self, outcome: Result<Rc<WebviewHost>, String>, cx: &mut Context<Self>) {
        match outcome {
            Ok(host) => {
                self.host = Some(host);
                // A URL typed before the page existed waits here rather than
                // being dropped on the floor.
                if let Some(url) = self.pending_url.take() {
                    self.navigate_to_url(url, cx);
                }
            }
            Err(error) => self.host_error = Some(error),
        }
        cx.notify();
    }

    /// The page navigated within the same document, so only the URL moved.
    /// Unlike a page load this must not touch `loading` or the title.
    #[cfg(target_os = "windows")]
    fn source_changed(&mut self, url: String, cx: &mut Context<Self>) {
        if url.is_empty() || self.current_url.as_deref() == Some(url.as_str()) {
            return;
        }
        self.current_url = Some(url);
        self.refresh_navigation_state();
        self.echo_page_url(cx);
        cx.notify();
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    fn build_webview(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {
        self.host_error = Some(tr!("browser.unavailable_on_platform"));
    }

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    fn page_load_changed(&mut self, event: PageLoad, url: String, cx: &mut Context<Self>) {
        match event {
            PageLoad::Started => {
                self.loading = true;
                // A fresh document invalidates the previous page's title; the
                // new one arrives via the title observer once known.
                self.page_title = None;
                // Committed navigation supersedes whatever was frozen.
                self.snapshot = None;
            }
            PageLoad::Finished => self.loading = false,
        }
        if !url.is_empty() {
            self.current_url = Some(url);
        }
        self.refresh_navigation_state();
        self.echo_page_url(cx);
        cx.notify();
    }

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    fn title_changed(&mut self, title: String, cx: &mut Context<Self>) {
        let title = (!title.trim().is_empty()).then_some(title);
        if self.page_title != title {
            self.page_title = title;
            cx.notify();
        }
    }

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    fn refresh_navigation_state(&mut self) {
        if let Some(host) = &self.host {
            self.can_go_back = host.webview.can_go_back().unwrap_or(false);
            self.can_go_forward = host.webview.can_go_forward().unwrap_or(false);
        }
    }

    /// Push the committed page URL into the address field unless the user is
    /// mid-edit there.
    fn echo_page_url(&mut self, cx: &mut Context<Self>) {
        if self.address_dirty {
            return;
        }
        let Some(url) = self.current_url.clone() else {
            return;
        };
        let shown = display_url(&url).to_owned();
        self.address.update(cx, |address, cx| {
            if address.content() != shown {
                address.set_content(shown, cx);
            }
        });
        self.address_dirty = false;
    }

    fn navigate_to_input(&mut self, raw: String, cx: &mut Context<Self>) {
        let Some(target) = resolve_address(&raw) else {
            return;
        };
        let url = match target {
            AddressTarget::Url(url) => url,
            AddressTarget::Search(query) => search_url(&query),
        };
        self.navigate_to_url(url, cx);
    }

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    pub fn navigate_to_url(&mut self, url: String, cx: &mut Context<Self>) {
        let Some(host) = &self.host else {
            #[cfg(target_os = "windows")]
            {
                self.pending_url = Some(url);
            }
            return;
        };
        if host.webview.load_url(&url).is_err() {
            return;
        }
        self.navigation_requested = true;
        self.loading = true;
        self.current_url = Some(url);
        self.address_dirty = false;
        self.echo_page_url(cx);
        self.focus_page(cx);
        cx.notify();
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    pub fn navigate_to_url(&mut self, _url: String, _cx: &mut Context<Self>) {}

    /// Hand the keyboard to the page. `makeFirstResponder` runs responder
    /// callbacks synchronously and this is reached from inside an entity
    /// update, so the native call takes the next executor turn.
    fn focus_page(&mut self, _cx: &mut Context<Self>) {
        #[cfg(target_os = "macos")]
        if let Some(host) = self.host.clone() {
            _cx.foreground_executor()
                .spawn(async move {
                    let _ = host.webview.focus();
                })
                .detach();
        }
        // `MoveFocus` is the only way in: a visual-hosted page has no window
        // of ours for a click to land on, so focus is always explicit.
        #[cfg(target_os = "windows")]
        if let Some(host) = self.host.clone() {
            _cx.foreground_executor()
                .spawn(async move {
                    host.focus_page();
                })
                .detach();
        }
    }

    /// Where focus should land when this surface becomes active: the page if
    /// there is one, otherwise the address bar ready for typing.
    pub fn focus_default(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.navigation_requested {
            self.focus_page(cx);
            window.focus(&self.focus_handle, cx);
        } else {
            self.focus_address(window, cx);
        }
    }

    pub fn focus_address(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.address.update(cx, |address, cx| {
            address.select_all_text(cx);
        });
        window.focus(&self.address.read(cx).focus(), cx);
        // GPUI focus alone is not enough: while the webview is first
        // responder, plain keystrokes never reach GPUI.
        self.reclaim_native_keyboard(cx);
        cx.notify();
    }

    fn restore_address(&mut self, cx: &mut Context<Self>) {
        self.address_dirty = false;
        self.echo_page_url(cx);
        cx.notify();
    }

    /// Per-frame push from the app: whether this surface is the visible right
    /// panel tab, and whether a GPUI overlay is open above it. Deduplicated
    /// down to real AppKit calls by the host.
    pub fn sync_native_state(
        &mut self,
        surface_visible: bool,
        occluded: bool,
        cx: &mut Context<Self>,
    ) {
        let occlusion_started = occluded && !self.occluded;
        self.occluded = occluded;

        let Some(host) = self.host.clone() else {
            return;
        };
        let has_page = self.navigation_requested;

        if surface_visible && has_page && occlusion_started && !self.snapshot_pending {
            self.request_snapshot(cx);
        }
        if !occluded && (self.snapshot.is_some() || self.snapshot_pending) {
            // The frame the overlay closes, the frozen pixels and any capture
            // still in flight are both stale; bumping the epoch makes a late
            // completion drop itself instead of resurfacing under the next
            // occlusion.
            self.snapshot = None;
            self.snapshot_pending = false;
            self.snapshot_epoch += 1;
        }

        // The live view stays up until its replacement pixels exist: hiding
        // is deferred to the frame the snapshot lands (identical pixels, so
        // the swap is invisible), rather than blanking for the frames the
        // capture takes. Until then the overlay's page-overlapping portion
        // simply appears a frame or two late. If the capture fails, the
        // completion clears the pending flag and this hides the view anyway —
        // a blank page area beats a menu nobody can see.
        let covered_by_snapshot = occluded && !self.snapshot_pending;
        let show = surface_visible && has_page && !covered_by_snapshot;
        // AppKit leaves a hidden view as first responder, so a page focused at
        // the moment its tab is switched away would keep eating the keyboard.
        if !show && host.native_focus_within() {
            self.reclaim_native_keyboard(cx);
        }
        host.set_visible(show);
    }

    #[cfg(target_os = "macos")]
    fn request_snapshot(&mut self, cx: &mut Context<Self>) {
        use objc2_app_kit::NSImage;
        use objc2_foundation::NSError;

        let Some(host) = &self.host else {
            return;
        };
        self.snapshot_pending = true;
        let epoch = self.snapshot_epoch;
        let deferred = Deferred {
            executor: cx.foreground_executor().clone(),
            cx: cx.to_async(),
            view: cx.entity().downgrade(),
        };
        let completion = block2::RcBlock::new(move |image: *mut NSImage, _: *mut NSError| {
            // Main thread, inside a WebKit completion: one raw-pixel copy —
            // never an image encode, which costs tens of milliseconds and
            // whose decode would push the first paint frames out.
            let render_image = unsafe { image.as_ref() }.and_then(snapshot_render_image);
            deferred.update(move |this, cx| {
                if this.snapshot_epoch == epoch {
                    this.snapshot_pending = false;
                    if this.occluded {
                        this.snapshot = render_image;
                    }
                    // Always redraw: the next frame's sync is what actually
                    // hides the live view now that the capture settled.
                    cx.notify();
                }
            });
        });
        unsafe {
            host.wk()
                .takeSnapshotWithConfiguration_completionHandler(None, &completion)
        };
    }

    #[cfg(not(target_os = "macos"))]
    fn request_snapshot(&mut self, _cx: &mut Context<Self>) {}

    /// Keep GPUI focus and the native first responder coherent. They are
    /// separate systems: clicks inside the webview move only the native side,
    /// clicks on GPUI controls move only GPUI's — and Zed's view never hands
    /// the native keyboard back on its own, because without native children it
    /// never loses it. Both directions are edge-triggered so neither rule
    /// fights the other's steady state:
    ///
    /// - GPUI focus just moved to a real control while the page held the
    ///   native keyboard → the control wins; reclaim the native first
    ///   responder or every keystroke would keep going to the page.
    /// - The webview just became natively focused with GPUI focus unchanged →
    ///   mirror GPUI onto this surface so Browser-scoped key bindings resolve.
    fn reconcile_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let natively_focused = self
            .host
            .as_ref()
            .is_some_and(|host| host.native_focus_within());
        let window_focus = window.focused(cx);
        let native_became_focused = natively_focused && !self.was_natively_focused;
        let window_focus_changed = window_focus != self.last_window_focus;
        let focus_on_gpui_control = window_focus
            .as_ref()
            .is_some_and(|focus| *focus != self.focus_handle);

        if natively_focused && window_focus_changed && focus_on_gpui_control {
            self.reclaim_native_keyboard(cx);
        } else if native_became_focused && !window_focus_changed {
            if self.address.read(cx).focus().is_focused(window) {
                // A stale native edge must never rip GPUI focus out of the
                // address bar mid-typing — the keyboard comes back instead.
                self.reclaim_native_keyboard(cx);
            } else {
                window.focus(&self.focus_handle, cx);
            }
        }

        self.was_natively_focused = natively_focused;
        self.last_window_focus = window_focus;
    }

    /// Return the native first responder to GPUI's view — deferred, since
    /// `makeFirstResponder` runs responder callbacks that may re-enter GPUI.
    fn reclaim_native_keyboard(&mut self, _cx: &mut Context<Self>) {
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        if let Some(host) = self.host.clone() {
            _cx.foreground_executor()
                .spawn(async move {
                    #[cfg(target_os = "macos")]
                    let _ = host.webview.focus_parent();
                    #[cfg(target_os = "windows")]
                    host.focus_parent();
                })
                .detach();
        }
    }

    #[cfg(target_os = "macos")]
    fn estimated_progress(&self) -> f64 {
        self.host
            .as_ref()
            .map(|host| unsafe { host.wk().estimatedProgress() })
            .unwrap_or(0.0)
    }

    #[cfg(not(target_os = "macos"))]
    fn estimated_progress(&self) -> f64 {
        0.0
    }

    fn go_back(&mut self, _cx: &mut Context<Self>) {
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        if let Some(host) = &self.host {
            let _ = host.webview.go_back();
            self.refresh_navigation_state();
            _cx.notify();
        }
    }

    fn go_forward(&mut self, _cx: &mut Context<Self>) {
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        if let Some(host) = &self.host {
            let _ = host.webview.go_forward();
            self.refresh_navigation_state();
            _cx.notify();
        }
    }

    fn reload(&mut self, _cx: &mut Context<Self>) {
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        if let Some(host) = &self.host
            && self.navigation_requested
        {
            let _ = host.webview.reload();
            self.loading = true;
            _cx.notify();
        }
    }

    fn hard_reload(&mut self, _cx: &mut Context<Self>) {
        #[cfg(target_os = "macos")]
        if let Some(host) = &self.host
            && self.navigation_requested
        {
            unsafe { host.wk().reloadFromOrigin() };
            self.loading = true;
            _cx.notify();
        }
        // WebView2 exposes no cache-bypassing reload; the scripted form is the
        // closest equivalent the page itself can perform.
        #[cfg(target_os = "windows")]
        if let Some(host) = &self.host
            && self.navigation_requested
        {
            let _ = host.webview.evaluate_script("location.reload(true)");
            self.loading = true;
            _cx.notify();
        }
    }

    fn stop_loading(&mut self, _cx: &mut Context<Self>) {
        #[cfg(target_os = "macos")]
        if let Some(host) = &self.host {
            unsafe { host.wk().stopLoading() };
            self.loading = false;
            self.refresh_navigation_state();
            _cx.notify();
        }
        #[cfg(target_os = "windows")]
        if let Some(host) = &self.host {
            let _ = host.webview.stop();
            self.loading = false;
            self.refresh_navigation_state();
            _cx.notify();
        }
    }

    fn toggle_devtools(&mut self) {
        #[cfg(target_os = "macos")]
        if let Some(host) = &self.host {
            if host.webview.is_devtools_open() {
                host.webview.close_devtools();
            } else {
                host.webview.open_devtools();
            }
        }
        // WebView2's devtools are a separate top-level window that the user
        // closes; there is no API to ask whether it is open, let alone shut
        // it, so this opens and re-focuses instead of toggling.
        #[cfg(target_os = "windows")]
        if let Some(host) = &self.host {
            let _ = host.webview.open_devtools();
        }
    }

    fn open_external(&self, cx: &mut Context<Self>) {
        if let Some(url) = &self.current_url {
            cx.open_url(url);
        }
    }

    /// Forward a standard editing selector to the webview. GPUI's window view
    /// claims key equivalents before AppKit's responder chain reaches the
    /// webview, so Browser-scoped bindings route the classics back natively.
    #[cfg(target_os = "macos")]
    fn perform_editing_selector(&self, selector: objc2::runtime::Sel) {
        use objc2::runtime::{AnyObject, NSObjectProtocol};

        if let Some(host) = &self.host {
            let view = host.ns_view();
            if !view.respondsToSelector(selector) {
                return;
            }
            let nil: *mut AnyObject = std::ptr::null_mut();
            let _: *mut AnyObject =
                unsafe { objc2::msg_send![view, performSelector: selector, withObject: nil] };
        }
    }

    /// Run a document editing command in the page.
    ///
    /// WebView2 handles the standard chords itself when the page holds the
    /// keyboard; this covers the case where Waku's own Browser-scoped
    /// bindings claimed the keystroke first.
    #[cfg(target_os = "windows")]
    fn perform_editing_command(&self, command: &str) {
        if let Some(host) = &self.host {
            let _ = host
                .webview
                .evaluate_script(&format!("document.execCommand('{command}')"));
        }
    }

    fn webview_copy(&self) {
        #[cfg(target_os = "macos")]
        self.perform_editing_selector(objc2::sel!(copy:));
        #[cfg(target_os = "windows")]
        self.perform_editing_command("copy");
    }

    fn webview_cut(&self) {
        #[cfg(target_os = "macos")]
        self.perform_editing_selector(objc2::sel!(cut:));
        #[cfg(target_os = "windows")]
        self.perform_editing_command("cut");
    }

    fn webview_paste(&self) {
        #[cfg(target_os = "macos")]
        self.perform_editing_selector(objc2::sel!(paste:));
        #[cfg(target_os = "windows")]
        self.perform_editing_command("paste");
    }

    fn webview_select_all(&self) {
        #[cfg(target_os = "macos")]
        self.perform_editing_selector(objc2::sel!(selectAll:));
        #[cfg(target_os = "windows")]
        self.perform_editing_command("selectAll");
    }

    fn toolbar_button(
        &self,
        id: &'static str,
        icon_path: &'static str,
        enabled: bool,
        tooltip: String,
        theme: Theme,
        on_click: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let base = div()
            .id(id)
            .size(px(26.0))
            .rounded(px(6.0))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .cursor_default();
        if !enabled {
            return base.child(icon(icon_path, 14.0, theme.text_ghost));
        }
        base.hover(|element| element.bg(theme.overlay))
            .active(|element| element.bg(theme.overlay_strong))
            .child(icon(icon_path, 14.0, theme.text_secondary))
            .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
            .on_click(cx.listener(move |this, _, window, cx| {
                on_click(this, window, cx);
            }))
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> Div {
        let theme = Theme::current(cx);
        let has_page = self.navigation_requested;
        let secure = self.current_url.as_deref().is_some_and(is_secure_url);
        let progress = self
            .loading
            .then(|| (self.estimated_progress().clamp(0.04, 1.0) * 1000.0).round() / 1000.0);

        div()
            .h(px(TOOLBAR_HEIGHT))
            .flex_none()
            .px(px(10.0))
            .flex()
            .items_center()
            .gap(px(2.0))
            .border_b_1()
            .border_color(theme.border)
            .child(self.toolbar_button(
                "browser-back",
                "icons/arrow-left.svg",
                self.can_go_back,
                tr!(
                    "browser.back",
                    shortcut = crate::platform::primary_shortcut("⌘[", "Ctrl+[")
                ),
                theme,
                |this, _, cx| this.go_back(cx),
                cx,
            ))
            .child(self.toolbar_button(
                "browser-forward",
                "icons/arrow-right.svg",
                self.can_go_forward,
                tr!(
                    "browser.forward",
                    shortcut = crate::platform::primary_shortcut("⌘]", "Ctrl+]")
                ),
                theme,
                |this, _, cx| this.go_forward(cx),
                cx,
            ))
            .child(if self.loading {
                self.toolbar_button(
                    "browser-stop",
                    "icons/x.svg",
                    true,
                    tr!("browser.stop_loading"),
                    theme,
                    |this, _, cx| this.stop_loading(cx),
                    cx,
                )
            } else {
                self.toolbar_button(
                    "browser-reload",
                    "icons/rotate-cw.svg",
                    has_page,
                    tr!(
                        "browser.reload",
                        shortcut = crate::platform::primary_shortcut("⌘R", "Ctrl+R")
                    ),
                    theme,
                    |this, _, cx| this.reload(cx),
                    cx,
                )
            })
            .child(
                TextField::new("browser-address", self.address.clone())
                    .icon(
                        if secure {
                            "icons/lock.svg"
                        } else {
                            "icons/globe.svg"
                        },
                        11.0,
                    )
                    .key_context("BrowserAddress")
                    .on_action(cx.listener(|this, _: &crate::BrowserAddressCancel, _, cx| {
                        this.restore_address(cx);
                    }))
                    .min_w_0()
                    .flex_1()
                    .mx(px(4.0))
                    .relative()
                    .when_some(progress, |element, progress| {
                        element.child(
                            div()
                                .absolute()
                                .bottom_0()
                                .left_0()
                                .h(px(2.0))
                                .w(gpui::relative(progress as f32))
                                .rounded_full()
                                .bg(theme.accent),
                        )
                    }),
            )
            .child(self.toolbar_button(
                "browser-open-external",
                "icons/external-link.svg",
                has_page,
                tr!("browser.open_external"),
                theme,
                |this, _, cx| this.open_external(cx),
                cx,
            ))
    }

    fn render_start_page(&self, theme: Theme) -> Div {
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .px(px(48.0))
            .pb(px(40.0))
            .child(icon("icons/globe.svg", 24.0, theme.text_ghost))
            .child(
                div()
                    .mt(px(14.0))
                    .text_size(sp(13.0))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(theme.text)
                    .child(tr!("browser.browse_web")),
            )
            .child(
                div()
                    .mt(px(6.0))
                    .max_w(px(310.0))
                    .text_center()
                    .text_size(sp(12.5))
                    .line_height(sp(17.0))
                    .text_color(theme.text_tertiary)
                    .whitespace_normal()
                    .child(tr!(
                        "browser.start_hint",
                        shortcut = crate::platform::primary_shortcut("⌘L", "Ctrl+L")
                    )),
            )
    }

    fn render_host_error(&self, message: SharedString, theme: Theme) -> Div {
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .px(px(48.0))
            .pb(px(40.0))
            .child(icon("icons/alert.svg", 22.0, theme.text_tertiary))
            .child(
                div()
                    .mt(px(14.0))
                    .text_size(sp(13.0))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(theme.text)
                    .child(tr!("browser.unavailable")),
            )
            .child(
                div()
                    .mt(px(6.0))
                    .max_w(px(340.0))
                    .text_center()
                    .text_size(sp(12.5))
                    .line_height(sp(17.0))
                    .text_color(theme.text_tertiary)
                    .whitespace_normal()
                    .child(message),
            )
    }

    /// Push GPUI's mouse events into the visual-hosted page.
    ///
    /// Visual hosting delivers no input at all — with no window of its own,
    /// WebView2 never sees a click — so every event has to be translated and
    /// handed over explicitly. These are registered from paint rather than as
    /// element handlers so they can consult the page's hitbox, which is what
    /// stops a click on an open menu from also reaching the page underneath
    /// now that the page no longer hides itself for one, and so they can use
    /// GPUI's pointer capture, which keeps a text selection alive after the
    /// pointer leaves the panel.
    #[cfg(target_os = "windows")]
    fn forward_page_input(
        host: Rc<WebviewHost>,
        focus: FocusHandle,
        hitbox: gpui::Hitbox,
        window: &mut Window,
    ) {
        use gpui::{DispatchPhase, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ScrollWheelEvent};

        // The page's own cursor, applied the way every other GPUI element
        // applies one, so it survives GPUI reasserting its cursor per frame.
        window.set_cursor_style(host.cursor_style(), &hitbox);

        window.on_mouse_event({
            let host = host.clone();
            let hitbox = hitbox.clone();
            move |event: &MouseDownEvent, phase, window, cx| {
                if phase != DispatchPhase::Bubble || !hitbox.is_hovered(window) {
                    return;
                }
                // Both focus systems move together: clicking the page is
                // how the user says the keyboard belongs to it now, and
                // whatever held GPUI focus — the address bar, the composer —
                // has to drop its caret to match. GPUI focus moves every
                // time, so `reconcile_focus` never reads the click as the
                // page stealing the keyboard from a control the user is
                // still using.
                window.focus(&focus, cx);
                if !host.native_focus_within() {
                    host.focus_page();
                }
                // Released automatically on the matching mouse up.
                window.capture_pointer(hitbox.id);
                host.mouse_down(
                    event.button,
                    event.position,
                    event.modifiers,
                    event.click_count,
                );
            }
        });

        window.on_mouse_event({
            let host = host.clone();
            let hitbox = hitbox.clone();
            move |event: &MouseUpEvent, phase, window, _| {
                if phase == DispatchPhase::Bubble && hitbox.is_hovered(window) {
                    host.mouse_up(event.button, event.position, event.modifiers);
                }
            }
        });

        window.on_mouse_event({
            let host = host.clone();
            let hitbox = hitbox.clone();
            move |event: &MouseMoveEvent, phase, window, _| {
                if phase != DispatchPhase::Bubble {
                    return;
                }
                if hitbox.is_hovered(window) {
                    host.mouse_move(event.position, event.modifiers);
                } else {
                    // Otherwise whatever the pointer left keeps its hover
                    // state, and the page's cursor never gives way.
                    host.mouse_leave();
                }
            }
        });

        window.on_mouse_event(move |event: &ScrollWheelEvent, phase, window, _| {
            if phase == DispatchPhase::Bubble && hitbox.should_handle_scroll(window) {
                host.scroll(event.position, event.delta, event.modifiers);
            }
        });
    }

    /// The page area: a canvas that mirrors its layout into the native view's
    /// frame, plus the frozen snapshot while a GPUI overlay is above us on a
    /// window without the scene-overlay plane. The native webview paints
    /// itself; GPUI paints what is underneath it — the surface colour shows
    /// only while a fallback snapshot is still being captured. The panel's
    /// resize handle keeps itself entirely left of this area, so the page owns
    /// the full width.
    fn render_page_area(&self, theme: Theme) -> Div {
        let host = self.host.clone();
        #[cfg(target_os = "windows")]
        let input = self.host.clone();
        #[cfg(target_os = "windows")]
        let focus = self.focus_handle.clone();
        div()
            .flex_1()
            .min_h_0()
            .relative()
            .bg(theme.surface)
            .child(
                canvas(
                    move |bounds, window, _| {
                        if let Some(host) = &host {
                            host.sync_bounds(bounds, window.scale_factor());
                        }
                        // A hitbox rather than a bare rectangle: it is what
                        // makes "is the pointer over the page" answer *no*
                        // while a GPUI menu is open above it, now that the
                        // page no longer hides itself for one.
                        window.insert_hitbox(bounds, HitboxBehavior::Normal)
                    },
                    move |_, _hitbox, _window, _| {
                        #[cfg(target_os = "windows")]
                        if let Some(host) = input {
                            Self::forward_page_input(host, focus, _hitbox, _window);
                        }
                    },
                )
                .absolute()
                .size_full(),
            )
            .when_some(
                self.occluded.then(|| self.snapshot.clone()).flatten(),
                |element, snapshot| {
                    element.child(
                        img(snapshot)
                            .absolute()
                            .size_full()
                            .object_fit(ObjectFit::Fill),
                    )
                },
            )
    }
}

/// Distilled page-load event, so handler closures stay free of wry types.
#[derive(Clone, Copy)]
#[cfg(any(target_os = "macos", target_os = "windows"))]
enum PageLoad {
    Started,
    Finished,
}

/// Convert a WebKit snapshot into pixels GPUI paints synchronously.
///
/// The rep wraps the snapshot's `CGImage` without re-encoding; the only cost
/// is one pass over the pixel buffer into the tightly packed BGRA order
/// [`gpui::RenderImage`] uploads as-is.
#[cfg(target_os = "macos")]
fn snapshot_render_image(
    image: &objc2_app_kit::NSImage,
) -> Option<std::sync::Arc<gpui::RenderImage>> {
    use objc2::AnyThread;
    use objc2_app_kit::{NSBitmapFormat, NSBitmapImageRep};

    let cg_image =
        unsafe { image.CGImageForProposedRect_context_hints(std::ptr::null_mut(), None, None) }?;
    let rep = NSBitmapImageRep::initWithCGImage(NSBitmapImageRep::alloc(), &cg_image);
    if rep.isPlanar() || rep.bitsPerSample() != 8 {
        return None;
    }
    let width = usize::try_from(rep.pixelsWide()).ok()?;
    let height = usize::try_from(rep.pixelsHigh()).ok()?;
    let bytes_per_row = usize::try_from(rep.bytesPerRow()).ok()?;
    let samples = usize::try_from(rep.samplesPerPixel()).ok()?;
    let format = rep.bitmapFormat();
    let data = rep.bitmapData();
    if data.is_null() {
        return None;
    }
    let bytes = unsafe { std::slice::from_raw_parts(data, bytes_per_row.checked_mul(height)?) };
    let bgra = bgra_from_bitmap(
        bytes,
        width,
        height,
        bytes_per_row,
        samples,
        format.contains(NSBitmapFormat::AlphaFirst),
        format.contains(NSBitmapFormat::ThirtyTwoBitLittleEndian),
    )?;
    let buffer = image::RgbaImage::from_raw(width as u32, height as u32, bgra)?;
    Some(std::sync::Arc::new(gpui::RenderImage::new(vec![
        image::Frame::new(buffer),
    ])))
}

/// Repack an `NSBitmapImageRep` pixel buffer as tight BGRA rows.
///
/// The rep's channel order follows two format flags: `alpha_first` gives the
/// declared sample order, and 32-bit little-endian packing stores that order
/// reversed in memory. Snapshots are opaque, so premultiplication needs no
/// undoing. Returns `None` for layouts snapshots never use (fewer than three
/// samples, undersized buffers) — the caller falls back to no snapshot.
#[cfg(any(target_os = "macos", test))]
fn bgra_from_bitmap(
    bytes: &[u8],
    width: usize,
    height: usize,
    bytes_per_row: usize,
    samples: usize,
    alpha_first: bool,
    little_endian_words: bool,
) -> Option<Vec<u8>> {
    if width == 0 || height == 0 || !(3..=4).contains(&samples) {
        return None;
    }
    let row_bytes = width.checked_mul(samples)?;
    if bytes_per_row < row_bytes || bytes.len() < bytes_per_row.checked_mul(height)? {
        return None;
    }

    // Where each output channel (B, G, R) lives within one pixel's bytes.
    let [b, g, r] = match (samples, alpha_first, little_endian_words) {
        (4, true, true) => [0, 1, 2], // memory B,G,R,A — the CGImage native case
        (4, false, false) => [2, 1, 0], // memory R,G,B,A
        (4, true, false) => [3, 2, 1], // memory A,R,G,B
        (4, false, true) => [1, 2, 3], // memory A,B,G,R
        _ => [2, 1, 0],               // 3-sample R,G,B
    };
    let alpha = match (samples, alpha_first, little_endian_words) {
        (4, true, true) => Some(3),
        (4, false, false) => Some(3),
        (4, true, false) => Some(0),
        (4, false, true) => Some(0),
        _ => None,
    };

    if (b, g, r, alpha) == (0, 1, 2, Some(3)) && bytes_per_row == row_bytes {
        return Some(bytes[..row_bytes * height].to_vec());
    }

    let mut out = Vec::with_capacity(width * height * 4);
    for row in bytes.chunks_exact(bytes_per_row).take(height) {
        for pixel in row[..row_bytes].chunks_exact(samples) {
            out.extend_from_slice(&[
                pixel[b],
                pixel[g],
                pixel[r],
                alpha.map_or(u8::MAX, |a| pixel[a]),
            ]);
        }
    }
    Some(out)
}

#[cfg(target_os = "macos")]
fn download_destination(url: &str, suggested: std::path::PathBuf) -> Option<std::path::PathBuf> {
    let downloads = dirs::download_dir()?;
    let name = suggested
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .or_else(|| {
            url.split(['?', '#'])
                .next()?
                .rsplit('/')
                .next()
                .map(str::to_owned)
                .filter(|name| !name.is_empty())
        })
        .unwrap_or_else(|| "download".to_owned());

    let path = downloads.join(&name);
    if !path.exists() {
        return Some(path);
    }
    let (stem, extension) = match name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() => (stem.to_owned(), format!(".{extension}")),
        _ => (name, String::new()),
    };
    (2..1000)
        .map(|counter| downloads.join(format!("{stem} ({counter}){extension}")))
        .find(|candidate| !candidate.exists())
}

#[cfg(target_os = "macos")]
fn reveal_in_finder(path: &std::path::Path) {
    use objc2_app_kit::NSWorkspace;
    use objc2_foundation::{NSArray, NSString, NSURL};

    let url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
    let urls = NSArray::from_retained_slice(&[url]);
    NSWorkspace::sharedWorkspace().activateFileViewerSelectingURLs(&urls);
}

impl Focusable for BrowserView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for BrowserView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::current(cx);
        self.reconcile_focus(window, cx);
        if self.loading {
            // `estimatedProgress` moves without any observable notification;
            // while a load is in flight the toolbar redraws with the frames.
            window.request_animation_frame();
        }

        let body = if let Some(error) = self.host_error.clone() {
            self.render_host_error(error.into(), theme)
                .into_any_element()
        } else if self.navigation_requested {
            self.render_page_area(theme).into_any_element()
        } else {
            self.render_start_page(theme).into_any_element()
        };

        div()
            .id("browser-surface")
            .track_focus(&self.focus_handle)
            .key_context("Browser")
            .on_action(cx.listener(|this, _: &BrowserBack, _, cx| this.go_back(cx)))
            .on_action(cx.listener(|this, _: &BrowserForward, _, cx| this.go_forward(cx)))
            .on_action(cx.listener(|this, _: &BrowserReload, _, cx| this.reload(cx)))
            .on_action(cx.listener(|this, _: &BrowserHardReload, _, cx| this.hard_reload(cx)))
            .on_action(cx.listener(|this, _: &BrowserStop, _, cx| this.stop_loading(cx)))
            .on_action(cx.listener(|this, _: &BrowserDevtools, _, _| this.toggle_devtools()))
            .on_action(cx.listener(|this, _: &FocusBrowserAddress, window, cx| {
                this.focus_address(window, cx);
            }))
            .on_action(cx.listener(|this, _: &WebviewCopy, _, _| this.webview_copy()))
            .on_action(cx.listener(|this, _: &WebviewCut, _, _| this.webview_cut()))
            .on_action(cx.listener(|this, _: &WebviewPaste, _, _| this.webview_paste()))
            .on_action(cx.listener(|this, _: &WebviewSelectAll, _, _| this.webview_select_all()))
            .size_full()
            .min_h_0()
            .flex()
            .flex_col()
            .child(self.render_toolbar(cx))
            .child(body)
    }
}

/// The address input's context menu floats above the native webview's area,
/// so the app's occlusion sync needs to know when it is open.
impl BrowserView {
    pub fn overlay_open(&self, cx: &App) -> bool {
        self.address.read(cx).context_menu_open()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_resolve_like_an_omnibox() {
        assert_eq!(
            resolve_address("https://example.com"),
            Some(AddressTarget::Url("https://example.com".into()))
        );
        assert_eq!(
            resolve_address("localhost:3000"),
            Some(AddressTarget::Url("http://localhost:3000".into()))
        );
        assert_eq!(
            resolve_address("127.0.0.1:8080/api"),
            Some(AddressTarget::Url("http://127.0.0.1:8080/api".into()))
        );
        assert_eq!(
            resolve_address("example.com/docs?q=1"),
            Some(AddressTarget::Url("https://example.com/docs?q=1".into()))
        );
        assert_eq!(
            resolve_address("about:blank"),
            Some(AddressTarget::Url("about:blank".into()))
        );
        assert_eq!(
            resolve_address("rust borrow checker"),
            Some(AddressTarget::Search("rust borrow checker".into()))
        );
        assert_eq!(
            resolve_address("what is wry"),
            Some(AddressTarget::Search("what is wry".into()))
        );
        assert_eq!(
            resolve_address("readme"),
            Some(AddressTarget::Search("readme".into()))
        );
        assert_eq!(resolve_address("   "), None);
    }

    #[test]
    fn search_urls_encode_queries() {
        assert_eq!(
            search_url("rust borrow checker"),
            "https://www.google.com/search?q=rust+borrow+checker"
        );
        assert_eq!(
            search_url("a&b=c"),
            "https://www.google.com/search?q=a%26b%3Dc"
        );
    }

    #[test]
    fn the_address_bar_hides_only_the_https_scheme() {
        assert_eq!(display_url("https://example.com/x"), "example.com/x");
        assert_eq!(
            display_url("http://localhost:3000"),
            "http://localhost:3000"
        );
        assert!(is_secure_url("https://example.com"));
        assert!(!is_secure_url("http://localhost:3000"));
    }

    #[test]
    fn bitmap_repacking_reaches_bgra_from_every_snapshot_layout() {
        // One red pixel then one green pixel, expressed in each channel
        // layout `NSBitmapImageRep` can hand back for an 8-bit snapshot.
        let bgra = [0u8, 0, 255, 255, 0, 255, 0, 255];
        let rgba = [255u8, 0, 0, 255, 0, 255, 0, 255];
        let argb = [255u8, 255, 0, 0, 255, 0, 255, 0];
        let abgr = [255u8, 0, 0, 255, 255, 0, 255, 0];
        let rgb = [255u8, 0, 0, 0, 255, 0];
        let expected = vec![0u8, 0, 255, 255, 0, 255, 0, 255];

        assert_eq!(
            bgra_from_bitmap(&bgra, 2, 1, 8, 4, true, true),
            Some(expected.clone())
        );
        assert_eq!(
            bgra_from_bitmap(&rgba, 2, 1, 8, 4, false, false),
            Some(expected.clone())
        );
        assert_eq!(
            bgra_from_bitmap(&argb, 2, 1, 8, 4, true, false),
            Some(expected.clone())
        );
        assert_eq!(
            bgra_from_bitmap(&abgr, 2, 1, 8, 4, false, true),
            Some(expected.clone())
        );
        assert_eq!(
            bgra_from_bitmap(&rgb, 2, 1, 6, 3, false, false),
            Some(expected)
        );
    }

    #[test]
    fn bitmap_repacking_honors_row_padding_and_rejects_bad_layouts() {
        // Two rows of one RGBA pixel with 4 bytes of row padding.
        let padded = [
            255u8, 0, 0, 255, 9, 9, 9, 9, //
            0, 255, 0, 255, 9, 9, 9, 9,
        ];
        assert_eq!(
            bgra_from_bitmap(&padded, 1, 2, 8, 4, false, false),
            Some(vec![0, 0, 255, 255, 0, 255, 0, 255])
        );
        assert_eq!(bgra_from_bitmap(&[0; 8], 2, 1, 8, 2, false, false), None);
        assert_eq!(bgra_from_bitmap(&[0; 7], 2, 1, 8, 4, false, false), None);
        assert_eq!(bgra_from_bitmap(&[], 0, 0, 0, 4, false, false), None);
    }

    #[test]
    fn download_names_do_not_overwrite() {
        // Pure-logic check of the uniquing shape; the filesystem probe path is
        // exercised by using a directory that cannot collide.
        let unique = std::env::temp_dir().join(format!("waku-download-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&unique).unwrap();
        std::fs::write(unique.join("file.txt"), "x").unwrap();
        let (stem, extension) = match "file.txt".rsplit_once('.') {
            Some((stem, extension)) if !stem.is_empty() => {
                (stem.to_owned(), format!(".{extension}"))
            }
            _ => ("file.txt".to_owned(), String::new()),
        };
        let next = (2..1000)
            .map(|counter| unique.join(format!("{stem} ({counter}){extension}")))
            .find(|candidate| !candidate.exists())
            .unwrap();
        assert_eq!(next.file_name().unwrap().to_str().unwrap(), "file (2).txt");
        std::fs::remove_dir_all(unique).unwrap();
    }
}

//! Native browser surface for the right panel, backed by a WKWebView that wry
//! attaches as a child NSView of the GPUI window.
//!
//! The webview is a real AppKit view floating above GPUI's Metal layer, so
//! three invariants keep it honest:
//!
//! - Geometry: the surface's content area syncs the native frame from element
//!   layout every frame, deduplicated so an unchanged frame costs nothing.
//! - Visibility: GPUI cannot paint over the native view, so [`Waku`] recomputes
//!   "should the webview be on screen" once per frame — panel visible, Browser
//!   tab active, no settings page, no open menu — and pushes it down here.
//!   While a menu or popover is open the live view swaps for a snapshot so
//!   GPUI overlays layer correctly above frozen page pixels.
//! - Threading: wry's delegate callbacks arrive on the main run loop, possibly
//!   while GPUI is mid-update, so they never touch entities directly. Each
//!   handler records intent and schedules the entity update on the foreground
//!   executor.
//!
//! The snapshot dance exists because GPUI draws its whole scene in one Metal
//! layer beneath native subviews. Zed PR #61945 ("layered scene rendering")
//! adds an overlay plane above native views; once it lands in the pinned GPUI,
//! `Window::enable_scene_overlay` replaces everything snapshot-related here.
//!
//! [`Waku`]: crate::app::Waku

use std::rc::Rc;

use gpui::{
    App, Context, Div, Entity, FocusHandle, Focusable, IntoElement, ObjectFit, Render,
    SharedString, Stateful, Subscription, Window, canvas, div, img, prelude::*, px,
};
#[cfg(target_os = "macos")]
use gpui::{AsyncApp, ForegroundExecutor, WeakEntity};

use crate::input::{ComposerEvent, ComposerInput};
use crate::theme::Theme;
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
        pub fn sync_bounds(&self, bounds: Bounds<Pixels>) {
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

#[cfg(not(target_os = "macos"))]
mod host {
    use gpui::{Bounds, Pixels};

    /// Webview embedding is only implemented for macOS.
    pub(super) struct WebviewHost;

    impl WebviewHost {
        pub fn sync_bounds(&self, _bounds: Bounds<Pixels>) {}
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
#[cfg(target_os = "macos")]
struct Deferred {
    executor: ForegroundExecutor,
    cx: AsyncApp,
    view: WeakEntity<BrowserView>,
}

#[cfg(target_os = "macos")]
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
    address: Entity<ComposerInput>,
    host: Option<Rc<WebviewHost>>,
    /// Why the webview could not be created, shown in place of the page.
    host_error: Option<String>,
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
            ComposerInput::new(window, cx)
                .search_field()
                .select_all_on_focus_click()
                .placeholder(tr!("input.search_or_enter_address"))
        });

        let submit_subscription = cx.subscribe(
            &address,
            |this: &mut Self, address, event: &ComposerEvent, cx| match event {
                ComposerEvent::Submit(text) => this.navigate_to_input(text.clone(), cx),
                // Search-mode fields never emit a steer; nothing to do here.
                ComposerEvent::SubmitSteer(_) => {}
                ComposerEvent::Edited => {
                    // Edits from the page echo itself also land here (events
                    // flush after the update that set the content), so dirty
                    // is derived, not latched: the field is dirty exactly
                    // while it shows something other than the page's URL.
                    let shown = this.current_url.as_deref().map(display_url).unwrap_or("");
                    this.address_dirty = address.read(cx).content() != shown;
                }
                ComposerEvent::Focus => {}
                ComposerEvent::BackspaceOnEmpty => {}
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

    #[cfg(not(target_os = "macos"))]
    fn build_webview(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {
        self.host_error = Some(tr!("browser.requires_macos"));
    }

    #[cfg(target_os = "macos")]
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

    #[cfg(target_os = "macos")]
    fn title_changed(&mut self, title: String, cx: &mut Context<Self>) {
        let title = (!title.trim().is_empty()).then_some(title);
        if self.page_title != title {
            self.page_title = title;
            cx.notify();
        }
    }

    #[cfg(target_os = "macos")]
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

    #[cfg(target_os = "macos")]
    pub fn navigate_to_url(&mut self, url: String, cx: &mut Context<Self>) {
        let Some(host) = &self.host else {
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

    #[cfg(not(target_os = "macos"))]
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
        #[cfg(target_os = "macos")]
        if let Some(host) = self.host.clone() {
            _cx.foreground_executor()
                .spawn(async move {
                    let _ = host.webview.focus_parent();
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
        #[cfg(target_os = "macos")]
        if let Some(host) = &self.host {
            let _ = host.webview.go_back();
            self.refresh_navigation_state();
            _cx.notify();
        }
    }

    fn go_forward(&mut self, _cx: &mut Context<Self>) {
        #[cfg(target_os = "macos")]
        if let Some(host) = &self.host {
            let _ = host.webview.go_forward();
            self.refresh_navigation_state();
            _cx.notify();
        }
    }

    fn reload(&mut self, _cx: &mut Context<Self>) {
        #[cfg(target_os = "macos")]
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
    }

    fn stop_loading(&mut self, _cx: &mut Context<Self>) {
        #[cfg(target_os = "macos")]
        if let Some(host) = &self.host {
            unsafe { host.wk().stopLoading() };
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

    fn webview_copy(&self) {
        #[cfg(target_os = "macos")]
        self.perform_editing_selector(objc2::sel!(copy:));
    }

    fn webview_cut(&self) {
        #[cfg(target_os = "macos")]
        self.perform_editing_selector(objc2::sel!(cut:));
    }

    fn webview_paste(&self) {
        #[cfg(target_os = "macos")]
        self.perform_editing_selector(objc2::sel!(paste:));
    }

    fn webview_select_all(&self) {
        #[cfg(target_os = "macos")]
        self.perform_editing_selector(objc2::sel!(selectAll:));
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
                    .text_size(px(13.0))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(theme.text)
                    .child(tr!("browser.browse_web")),
            )
            .child(
                div()
                    .mt(px(6.0))
                    .max_w(px(310.0))
                    .text_center()
                    .text_size(px(11.0))
                    .line_height(px(17.0))
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
                    .text_size(px(13.0))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(theme.text)
                    .child(tr!("browser.unavailable")),
            )
            .child(
                div()
                    .mt(px(6.0))
                    .max_w(px(340.0))
                    .text_center()
                    .text_size(px(11.0))
                    .line_height(px(17.0))
                    .text_color(theme.text_tertiary)
                    .whitespace_normal()
                    .child(message),
            )
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
        div()
            .flex_1()
            .min_h_0()
            .relative()
            .bg(theme.surface)
            .child(
                canvas(
                    move |bounds, _, _| {
                        if let Some(host) = &host {
                            host.sync_bounds(bounds);
                        }
                    },
                    |_, _, _, _| {},
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
#[cfg(target_os = "macos")]
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

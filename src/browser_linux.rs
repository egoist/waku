//! WebKitGTK composited into GPUI as a CPU snapshot.
//!
//! wry's child-window path needs an Xlib parent and a GTK main loop. GPUI's
//! Linux backend speaks xcb or Wayland, never Xlib, and is not GTK — so there
//! is nothing to reparent into. The supported answer on this platform is the
//! same shape as Windows visual hosting: keep a real WebKitGTK view off
//! screen, blit its pixels into a GPUI `RenderImage`, and forward mouse and
//! keyboard from the page hitbox by hand.
//!
//! A `GtkOffscreenWindow` owns the widget. Hardware acceleration is turned
//! off so the backing pixbuf is actually populated (accelerated compositing
//! often paints to a GL texture the offscreen window cannot read). GTK is
//! pumped from GPUI's thread alongside the window loop.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::Once;
use std::time::{Duration, Instant};

use gpui::{
    Bounds, Keystroke, Modifiers, MouseButton, NavigationDirection, Pixels, Point, RenderImage,
    ScrollDelta,
};
use gtk::gdk::{self, EventType, ModifierType, ScrollDirection};
use gtk::glib::object::ObjectExt;
use gtk::glib::translate::ToGlibPtr;
use gtk::prelude::*;
use gtk::{OffscreenWindow, gdk_pixbuf};
use webkit2gtk::{HardwareAccelerationPolicy, InputMethodContextExt, SettingsExt, WebViewExt};
use wry::{WebViewBuilderExtUnix, WebViewExtUnix};

use super::{Deferred, PageLoad, download_destination, reveal_in_finder};

const USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 \
     (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

pub fn pump_gtk() {
    if !gtk::is_initialized() {
        return;
    }
    while gtk::events_pending() {
        gtk::main_iteration_do(false);
    }
}

fn ensure_gtk() -> Result<(), String> {
    static INIT: Once = Once::new();
    static ERROR: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

    INIT.call_once(|| {
        // WebKitGTK's default GL compositor often fails to produce a readable
        // buffer inside an OffscreenWindow, especially on Wayland. The CPU
        // path is the one that fills `gtk_offscreen_window_get_pixbuf`.
        if std::env::var_os("WEBKIT_DISABLE_COMPOSITING_MODE").is_none() {
            unsafe { std::env::set_var("WEBKIT_DISABLE_COMPOSITING_MODE", "1") };
        }
        if std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none() {
            unsafe { std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1") };
        }
        if let Err(error) = gtk::init() {
            *ERROR.lock().unwrap() = Some(error.to_string());
        }
    });

    match ERROR.lock().ok().and_then(|guard| guard.clone()) {
        Some(error) => Err(error),
        None if gtk::is_initialized() => Ok(()),
        None => Err("GTK failed to initialize".to_owned()),
    }
}

fn gdk_modifiers(modifiers: Modifiers) -> ModifierType {
    let mut state = ModifierType::empty();
    if modifiers.shift {
        state |= ModifierType::SHIFT_MASK;
    }
    if modifiers.control {
        state |= ModifierType::CONTROL_MASK;
    }
    if modifiers.alt {
        state |= ModifierType::MOD1_MASK;
    }
    if modifiers.platform {
        state |= ModifierType::SUPER_MASK;
    }
    state
}

fn pointer_device() -> Option<gdk::Device> {
    gdk::Display::default()
        .and_then(|display| display.default_seat())
        .and_then(|seat| seat.pointer())
}

fn keyboard_device() -> Option<gdk::Device> {
    gdk::Display::default()
        .and_then(|display| display.default_seat())
        .and_then(|seat| seat.keyboard())
}

fn attach_window(event: &mut gdk::Event, window: &gdk::Window) {
    let any = event.as_mut();
    any.window = window.to_glib_full();
    // WebKit drops events marked synthetic (`send_event != 0`).
    any.send_event = 0;
}

fn keyval_for(keystroke: &Keystroke) -> Option<gdk::keys::Key> {
    if !keystroke.modifiers.control && !keystroke.modifiers.alt && !keystroke.modifiers.platform {
        if let Some(text) = keystroke.key_char.as_deref() {
            let mut chars = text.chars();
            if let Some(ch) = chars.next()
                && chars.next().is_none()
                && !ch.is_control()
            {
                return Some(gdk::keys::Key::from_unicode(ch));
            }
        }
    }
    use gdk::keys::constants as key;
    Some(match keystroke.key.as_str() {
        "enter" | "return" => key::Return,
        "tab" => key::Tab,
        "escape" => key::Escape,
        "backspace" => key::BackSpace,
        "delete" => key::Delete,
        "space" => key::space,
        "left" => key::Left,
        "right" => key::Right,
        "up" => key::Up,
        "down" => key::Down,
        "home" => key::Home,
        "end" => key::End,
        "pageup" => key::Page_Up,
        "pagedown" => key::Page_Down,
        "insert" => key::Insert,
        other if other.chars().count() == 1 => gdk::keys::Key::from_unicode(other.chars().next()?),
        _ => return None,
    })
}

fn render_image_from_pixbuf(pixbuf: &gdk_pixbuf::Pixbuf) -> Option<Arc<RenderImage>> {
    let width = usize::try_from(pixbuf.width()).ok()?;
    let height = usize::try_from(pixbuf.height()).ok()?;
    if width == 0 || height == 0 {
        return None;
    }
    let stride = usize::try_from(pixbuf.rowstride()).ok()?;
    let channels = pixbuf.n_channels();
    if channels != 3 && channels != 4 {
        return None;
    }
    let pixels = unsafe { pixbuf.pixels() };
    let mut bgra = vec![0u8; width.checked_mul(height)?.checked_mul(4)?];
    for y in 0..height {
        let row = pixels.get(y.checked_mul(stride)?..)?;
        for x in 0..width {
            let i = x.checked_mul(channels as usize)?;
            let o = (y * width + x) * 4;
            let r = *row.get(i)?;
            let g = *row.get(i + 1)?;
            let b = *row.get(i + 2)?;
            let a = if channels == 4 { *row.get(i + 3)? } else { 255 };
            bgra[o] = b;
            bgra[o + 1] = g;
            bgra[o + 2] = r;
            bgra[o + 3] = a;
        }
    }
    let buffer = image::RgbaImage::from_raw(width as u32, height as u32, bgra)?;
    Some(Arc::new(RenderImage::new(vec![image::Frame::new(buffer)])))
}

pub(super) struct WebviewHost {
    pub webview: wry::WebView,
    offscreen: OffscreenWindow,
    widget: webkit2gtk::WebView,
    last_size: Cell<Option<(i32, i32)>>,
    origin: Cell<Point<Pixels>>,
    scale: Cell<f32>,
    visible: Cell<bool>,
    focused: Cell<bool>,
    entered: Cell<bool>,
    dirty: Rc<Cell<bool>>,
    frame: RefCell<Option<Arc<RenderImage>>>,
    last_char: RefCell<Option<(String, Instant)>>,
}

impl WebviewHost {
    fn new(webview: wry::WebView, offscreen: OffscreenWindow, dirty: Rc<Cell<bool>>) -> Self {
        let widget = WebViewExtUnix::webview(&webview);
        if let Some(settings) = WebViewExt::settings(&widget) {
            settings.set_hardware_acceleration_policy(HardwareAccelerationPolicy::Never);
        }
        widget.set_can_focus(true);
        widget.set_focus_on_click(true);
        offscreen.set_accept_focus(true);
        Self {
            webview,
            offscreen,
            widget,
            last_size: Cell::new(None),
            origin: Cell::new(Point::default()),
            scale: Cell::new(1.0),
            visible: Cell::new(false),
            focused: Cell::new(false),
            entered: Cell::new(false),
            dirty,
            frame: RefCell::new(None),
            last_char: RefCell::new(None),
        }
    }

    pub fn estimated_progress(&self) -> f64 {
        self.widget.estimated_load_progress()
    }

    pub fn live_frame(&self) -> Option<Arc<RenderImage>> {
        self.frame.borrow().clone()
    }

    /// Pump GTK, capture a new frame if the page dirtied, and report whether
    /// GPUI should redraw.
    pub fn present(&self) -> bool {
        pump_gtk();
        if !self.visible.get() {
            return false;
        }
        if !self.dirty.replace(false) && self.frame.borrow().is_some() {
            return false;
        }
        if let Some(image) = self.capture() {
            *self.frame.borrow_mut() = Some(image);
            true
        } else {
            self.dirty.set(true);
            false
        }
    }

    fn capture(&self) -> Option<Arc<RenderImage>> {
        render_image_from_pixbuf(&self.offscreen.pixbuf()?)
    }

    pub fn sync_bounds(&self, bounds: Bounds<Pixels>, scale: f32) {
        self.origin.set(bounds.origin);
        self.scale.set(scale);
        let width = (f32::from(bounds.size.width) * scale).round().max(1.0) as i32;
        let height = (f32::from(bounds.size.height) * scale).round().max(1.0) as i32;
        if self.last_size.get() == Some((width, height)) {
            return;
        }
        self.last_size.set(Some((width, height)));
        self.offscreen.set_size_request(width, height);
        self.offscreen.resize(width, height);
        self.widget.set_size_request(width, height);
        self.dirty.set(true);
    }

    pub fn set_visible(&self, visible: bool) {
        if self.visible.get() == visible {
            return;
        }
        self.visible.set(visible);
        if visible {
            self.dirty.set(true);
        } else {
            self.mouse_leave();
        }
    }

    pub fn native_focus_within(&self) -> bool {
        self.focused.get()
    }

    /// WebKit handles input on an inner child, not on `WebKitWebView`.
    /// `gtk_propagate_event` only walks parents, so we have to start there.
    fn input_widget(&self) -> gtk::Widget {
        fn deepest(widget: &gtk::Widget) -> gtk::Widget {
            let Some(container) = widget.downcast_ref::<gtk::Container>() else {
                return widget.clone();
            };
            for child in container.children() {
                if child.is_visible() {
                    return deepest(&child);
                }
            }
            widget.clone()
        }
        deepest(self.widget.upcast_ref())
    }

    pub fn focus_page(&self) {
        self.focused.set(true);
        self.widget.set_can_focus(true);
        // Keys are handled on WebKitWebView, not the innermost backing child.
        self.offscreen.set_focus(Some(&self.widget));
        self.widget.grab_focus();
        if let Some(im) = self.widget.input_method_context() {
            im.notify_focus_in();
        }
        let mut event = gdk::Event::new(EventType::FocusChange);
        if let Some(focus) = event.downcast_mut::<gdk::EventFocus>() {
            focus.as_mut().in_ = 1;
        }
        self.dispatch_on(
            event,
            None,
            self.widget.window().or_else(|| self.offscreen.window()),
        );
    }

    pub fn focus_parent(&self) {
        self.focused.set(false);
        if let Some(im) = self.widget.input_method_context() {
            im.notify_focus_out();
        }
        self.offscreen.set_focus(None::<&gtk::Widget>);
    }

    pub fn stop_loading(&self) {
        self.widget.stop_loading();
    }

    pub fn reload_from_origin(&self) {
        self.widget.reload_bypass_cache();
    }

    fn page_point(&self, position: Point<Pixels>) -> (f64, f64) {
        let origin = self.origin.get();
        let scale = self.scale.get() as f64;
        (
            ((f32::from(position.x) - f32::from(origin.x)) as f64 * scale).max(0.0),
            ((f32::from(position.y) - f32::from(origin.y)) as f64 * scale).max(0.0),
        )
    }

    fn event_window(&self) -> Option<gdk::Window> {
        self.input_widget()
            .window()
            .or_else(|| self.widget.window())
            .or_else(|| self.offscreen.window())
    }

    fn dispatch(&self, event: gdk::Event, device: Option<&gdk::Device>) {
        self.dispatch_on(event, device, self.event_window());
    }

    fn dispatch_on(
        &self,
        mut event: gdk::Event,
        device: Option<&gdk::Device>,
        window: Option<gdk::Window>,
    ) {
        let Some(window) = window else {
            return;
        };
        attach_window(&mut event, &window);
        if let Some(device) = device {
            event.set_device(Some(device));
        }
        let target = self.input_widget();
        // One path only. `main_do_event` plus `widget.event` turns one click
        // into a double-click (word-select) and one key into two characters.
        if target.has_window() {
            gtk::main_do_event(&mut event);
        } else {
            let _ = target.event(&event);
        }
        pump_gtk();
        self.dirty.set(true);
    }

    fn ensure_pointer_entered(&self, x: f64, y: f64, modifiers: Modifiers) {
        if self.entered.replace(true) {
            return;
        }
        let mut event = gdk::Event::new(EventType::EnterNotify);
        if let Some(crossing) = event.downcast_mut::<gdk::EventCrossing>() {
            let native = crossing.as_mut();
            native.x = x;
            native.y = y;
            native.x_root = x;
            native.y_root = y;
            native.state = gdk_modifiers(modifiers).bits();
            native.focus = glib_true();
            native.mode = {
                use gtk::glib::translate::IntoGlib;
                gdk::CrossingMode::Normal.into_glib()
            };
            native.detail = {
                use gtk::glib::translate::IntoGlib;
                gdk::NotifyType::Nonlinear.into_glib()
            };
        }
        self.dispatch(event, pointer_device().as_ref());
    }

    pub fn mouse_down(
        &self,
        button: MouseButton,
        position: Point<Pixels>,
        modifiers: Modifiers,
        click_count: usize,
    ) {
        let Some(n) = gdk_button(button) else {
            match button {
                MouseButton::Navigate(NavigationDirection::Back) => {
                    let _ = self.webview.go_back();
                }
                MouseButton::Navigate(NavigationDirection::Forward) => {
                    let _ = self.webview.go_forward();
                }
                _ => {}
            }
            return;
        };
        let kind = match click_count {
            0 | 1 => EventType::ButtonPress,
            2 => EventType::DoubleButtonPress,
            _ => EventType::TripleButtonPress,
        };
        let (x, y) = self.page_point(position);
        self.focus_page();
        self.ensure_pointer_entered(x, y, modifiers);
        let mut event = gdk::Event::new(kind);
        if let Some(button) = event.downcast_mut::<gdk::EventButton>() {
            let native = button.as_mut();
            native.button = n;
            native.x = x;
            native.y = y;
            native.x_root = x;
            native.y_root = y;
            native.state = gdk_modifiers(modifiers).bits();
        }
        self.dispatch(event, pointer_device().as_ref());
    }

    pub fn mouse_up(&self, button: MouseButton, position: Point<Pixels>, modifiers: Modifiers) {
        let Some(n) = gdk_button(button) else {
            return;
        };
        let (x, y) = self.page_point(position);
        let mut event = gdk::Event::new(EventType::ButtonRelease);
        if let Some(button) = event.downcast_mut::<gdk::EventButton>() {
            let native = button.as_mut();
            native.button = n;
            native.x = x;
            native.y = y;
            native.x_root = x;
            native.y_root = y;
            native.state = gdk_modifiers(modifiers).bits();
        }
        self.dispatch(event, pointer_device().as_ref());
    }

    pub fn mouse_move(&self, position: Point<Pixels>, modifiers: Modifiers) {
        let (x, y) = self.page_point(position);
        self.ensure_pointer_entered(x, y, modifiers);
        let mut event = gdk::Event::new(EventType::MotionNotify);
        if let Some(motion) = event.downcast_mut::<gdk::EventMotion>() {
            let native = motion.as_mut();
            native.x = x;
            native.y = y;
            native.x_root = x;
            native.y_root = y;
            native.state = gdk_modifiers(modifiers).bits();
        }
        self.dispatch(event, pointer_device().as_ref());
    }

    pub fn mouse_leave(&self) {
        if !self.entered.replace(false) {
            return;
        }
        let event = gdk::Event::new(EventType::LeaveNotify);
        self.dispatch(event, pointer_device().as_ref());
    }

    pub fn scroll(&self, position: Point<Pixels>, delta: ScrollDelta, modifiers: Modifiers) {
        let (dx, dy) = match delta {
            ScrollDelta::Lines(delta) => (delta.x as f64, delta.y as f64),
            ScrollDelta::Pixels(delta) => (
                f32::from(delta.x) as f64 / 20.0,
                f32::from(delta.y) as f64 / 20.0,
            ),
        };
        if dx == 0.0 && dy == 0.0 {
            return;
        }
        let (x, y) = self.page_point(position);
        self.ensure_pointer_entered(x, y, modifiers);
        let mut event = gdk::Event::new(EventType::Scroll);
        if let Some(scroll) = event.downcast_mut::<gdk::EventScroll>() {
            let native = scroll.as_mut();
            native.x = x;
            native.y = y;
            native.x_root = x;
            native.y_root = y;
            native.state = gdk_modifiers(modifiers).bits();
            native.direction = {
                use gtk::glib::translate::IntoGlib;
                ScrollDirection::Smooth.into_glib()
            };
            native.delta_x = -dx;
            native.delta_y = -dy;
        }
        self.dispatch(event, pointer_device().as_ref());
    }

    fn skip_duplicate_char(&self, text: &str, is_held: bool) -> bool {
        if is_held {
            return false;
        }
        let now = Instant::now();
        let mut last = self.last_char.borrow_mut();
        if let Some((prev, at)) = last.as_ref()
            && prev == text
            && now.duration_since(*at) < Duration::from_millis(40)
        {
            return true;
        }
        *last = Some((text.to_owned(), now));
        false
    }

    pub fn key_event(&self, keystroke: &Keystroke, pressed: bool, is_held: bool) {
        if pressed {
            if let Some(text) = printable_text(keystroke) {
                if self.skip_duplicate_char(&text, is_held) {
                    return;
                }
                if let Some(im) = self.widget.input_method_context() {
                    im.emit_by_name::<()>("committed", &[&text]);
                    pump_gtk();
                    self.dirty.set(true);
                    return;
                }
            }
        } else if printable_text(keystroke).is_some() {
            return;
        }

        let Some(keyval) = keyval_for(keystroke) else {
            return;
        };
        let kind = if pressed {
            EventType::KeyPress
        } else {
            EventType::KeyRelease
        };
        let (hardware_keycode, group) = keycode_for(keyval);
        let mut event = gdk::Event::new(kind);
        if let Some(key) = event.downcast_mut::<gdk::EventKey>() {
            let native = key.as_mut();
            native.keyval = *keyval;
            native.hardware_keycode = hardware_keycode;
            native.group = group;
            native.state = gdk_modifiers(keystroke.modifiers).bits();
            native.time = key_time();
        }
        if let Some(window) = self.widget.window().or_else(|| self.offscreen.window()) {
            attach_window(&mut event, &window);
        }
        if let Some(device) = keyboard_device() {
            event.set_device(Some(&device));
        }
        let _ = self.widget.event(&event);
        pump_gtk();
        self.dirty.set(true);
    }
}

fn printable_text(keystroke: &Keystroke) -> Option<String> {
    if keystroke.modifiers.control || keystroke.modifiers.alt || keystroke.modifiers.platform {
        return None;
    }
    if let Some(text) = keystroke.key_char.as_deref() {
        if !text.is_empty() && !text.chars().any(|ch| ch.is_control()) {
            return Some(text.to_owned());
        }
    }
    match keystroke.key.as_str() {
        "space" => Some(" ".to_owned()),
        other if other.chars().count() == 1 => {
            let ch = other.chars().next()?;
            (!ch.is_control()).then(|| other.to_owned())
        }
        _ => None,
    }
}

fn key_time() -> u32 {
    (gtk::glib::monotonic_time() / 1000) as u32
}

fn keycode_for(keyval: gdk::keys::Key) -> (u16, u8) {
    let Some(keymap) =
        gdk::Display::default().and_then(|display| gdk::Keymap::for_display(&display))
    else {
        return (0, 0);
    };
    keymap
        .entries_for_keyval(*keyval)
        .into_iter()
        .next()
        .map(|key| (key.keycode() as u16, key.group() as u8))
        .unwrap_or((0, 0))
}

fn glib_true() -> i32 {
    use gtk::glib::translate::IntoGlib;
    true.into_glib()
}

fn gdk_button(button: MouseButton) -> Option<u32> {
    match button {
        MouseButton::Left => Some(1),
        MouseButton::Middle => Some(2),
        MouseButton::Right => Some(3),
        MouseButton::Navigate(_) => None,
    }
}

pub(super) fn build_host(deferred: Deferred) -> Result<WebviewHost, String> {
    ensure_gtk()?;

    let offscreen = OffscreenWindow::new();
    offscreen.set_app_paintable(true);
    let container = gtk::Box::new(gtk::Orientation::Vertical, 0);
    offscreen.add(&container);

    let dirty = Rc::new(Cell::new(true));
    let on_page_load = deferred.clone();
    let on_title = deferred.clone();
    let on_new_window = deferred.clone();
    let on_draw = deferred.clone();
    let dirty_for_draw = dirty.clone();

    let built = wry::WebViewBuilder::new()
        .with_visible(true)
        .with_focused(false)
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
        .build_gtk(&container)
        .map_err(|error| error.to_string())?;

    offscreen.show_all();
    pump_gtk();

    let host = WebviewHost::new(built, offscreen, dirty);
    host.widget.connect_draw(move |_, _| {
        dirty_for_draw.set(true);
        on_draw.update(|_, cx| cx.notify());
        gtk::glib::Propagation::Proceed
    });
    Ok(host)
}

use gpui::{App, Global, Hsla, Window, WindowAppearance, hsla, rgb, transparent_black};

pub use waku_client::theme::ThemePreference;

pub const FONT_SANS: &str = "Inter";
pub const RADIUS_SM: f32 = 4.0;
pub const RADIUS_DF: f32 = 6.0;
pub const RADIUS_LG: f32 = 999.0;

fn resolves_to_dark(preference: ThemePreference, system_appearance: WindowAppearance) -> bool {
    match preference {
        ThemePreference::System => matches!(
            system_appearance,
            WindowAppearance::Dark | WindowAppearance::VibrantDark
        ),
        ThemePreference::Light => false,
        ThemePreference::Dark => true,
    }
}

fn native_override(preference: ThemePreference) -> Option<bool> {
    match preference {
        ThemePreference::System => None,
        ThemePreference::Light => Some(false),
        ThemePreference::Dark => Some(true),
    }
}

/// Native adapter for the Nucleus brand tokens. Existing field names describe
/// product roles while their values stay tied to the portable semantic system.
/// On macOS the sidebar tint is installed as a native layer above Sidebar
/// vibrancy, so its GPUI surface remains clear.
#[derive(Clone, Copy)]
pub struct Theme {
    pub is_dark: bool,
    pub canvas: Hsla,
    pub sidebar: Hsla,
    pub sidebar_drag_background: Hsla,
    pub sidebar_item_background: Hsla,
    pub surface: Hsla,
    pub raised: Hsla,
    pub composer: Hsla,
    pub inset: Hsla,
    /// Terminal screen surface: paper-white in light mode, near-black in dark.
    pub terminal: Hsla,
    pub overlay: Hsla,
    pub overlay_strong: Hsla,

    pub border: Hsla,
    pub border_strong: Hsla,
    pub sidebar_border: Hsla,

    pub text: Hsla,
    pub text_secondary: Hsla,
    pub text_tertiary: Hsla,
    pub text_ghost: Hsla,

    /// Nucleus primary blue, reserved for actions and selected brand states.
    pub accent: Hsla,
    pub primary_hover: Hsla,
    pub ring: Hsla,
    pub scrim: Hsla,
    pub resize_handle: Hsla,
    /// Meter fills in the usage panel. Quota-meter blue by convention;
    /// warning/danger take over as a lane fills.
    pub gauge: Hsla,

    /// Text-selection wash. Painted *under* the glyphs, so it stays
    /// translucent and deliberately reads as the familiar browser blue rather
    /// than as brand color.
    pub selection: Hsla,
    /// Inline `code` foreground and its rounded wash.
    pub code_text: Hsla,
    pub code_wash: Hsla,

    /// Light fill for primary buttons (send, allow), dark glyph on top.
    pub inverse: Hsla,
    pub on_inverse: Hsla,

    pub warning: Hsla,
    pub success: Hsla,
    pub favorite: Hsla,
    pub danger: Hsla,
    pub danger_soft: Hsla,
}

impl Theme {
    pub fn current(cx: &App) -> Self {
        if cx.has_global::<ActiveWakuTheme>() {
            cx.global::<ActiveWakuTheme>().0
        } else {
            Self::dark()
        }
    }

    pub fn dark() -> Self {
        Self {
            is_dark: true,
            canvas: rgb(0x070A0F).into(),
            sidebar: if cfg!(target_os = "macos") {
                transparent_black()
            } else {
                rgb(0x0C1115).into()
            },
            sidebar_drag_background: rgb(0x0C1115).into(),
            sidebar_item_background: rgb(0x222224).into(),
            surface: rgb(0x070A0F).into(),
            raised: rgb(0x070A0F).into(),
            composer: rgb(0x070A0F).into(),
            inset: rgb(0x0C1115).into(),
            terminal: rgb(0x070A0F).into(),
            overlay: rgb(0x222224).into(),
            overlay_strong: rgb(0x222224).into(),

            border: rgb(0x131519).into(),
            border_strong: rgb(0x16191F).into(),
            sidebar_border: rgb(0x131519).into(),

            text: rgb(0xFAFAFA).into(),
            text_secondary: rgb(0x9DA3AA).into(),
            text_tertiary: rgb(0x9DA3AA).into(),
            text_ghost: rgb(0x696969).into(),

            accent: rgb(0x3E63DD).into(),
            primary_hover: rgb(0x4E76F2).into(),
            ring: rgb(0x737373).into(),
            scrim: hsla(0.0, 0.0, 0.0, 0.50),
            resize_handle: rgb(0x3E63DD).into(),
            gauge: rgb(0x3E63DD).into(),

            selection: hsla(225.7 / 360.0, 0.706, 0.555, 0.35),
            code_text: rgb(0xFAFAFA).into(),
            code_wash: rgb(0x0C1115).into(),

            inverse: rgb(0x3E63DD).into(),
            on_inverse: rgb(0xEFF6FF).into(),

            warning: rgb(0xF59E0A).into(),
            success: rgb(0x089981).into(),
            favorite: rgb(0xF59E0A).into(),
            danger: rgb(0xF7525F).into(),
            danger_soft: hsla(355.0 / 360.0, 0.91, 0.65, 0.10),
        }
    }

    pub fn light() -> Self {
        Self {
            is_dark: false,
            canvas: rgb(0xFFFFFF).into(),
            sidebar: if cfg!(target_os = "macos") {
                transparent_black()
            } else {
                rgb(0xFCFCFC).into()
            },
            sidebar_drag_background: rgb(0xFCFCFC).into(),
            sidebar_item_background: rgb(0xF7F7F7).into(),
            surface: rgb(0xFFFFFF).into(),
            raised: rgb(0xFFFFFF).into(),
            composer: rgb(0xFFFFFF).into(),
            inset: rgb(0xFCFCFC).into(),
            terminal: rgb(0xFFFFFF).into(),
            overlay: rgb(0xF7F7F7).into(),
            overlay_strong: rgb(0xF7F7F7).into(),

            border: rgb(0xF5F5F5).into(),
            border_strong: rgb(0xF3F3F3).into(),
            sidebar_border: rgb(0xF5F5F5).into(),

            text: rgb(0x333333).into(),
            text_secondary: rgb(0x737373).into(),
            text_tertiary: rgb(0x737373).into(),
            text_ghost: rgb(0xABABAB).into(),

            accent: rgb(0x3E63DD).into(),
            primary_hover: rgb(0x2F50C8).into(),
            ring: rgb(0xA1A1A1).into(),
            scrim: hsla(0.0, 0.0, 0.0, 0.50),
            resize_handle: rgb(0x3E63DD).into(),
            gauge: rgb(0x3E63DD).into(),

            selection: hsla(225.7 / 360.0, 0.706, 0.555, 0.25),
            code_text: rgb(0x333333).into(),
            code_wash: rgb(0xFCFCFC).into(),

            inverse: rgb(0x3E63DD).into(),
            on_inverse: rgb(0xEFF6FF).into(),

            warning: rgb(0xF59E0A).into(),
            success: rgb(0x089981).into(),
            favorite: rgb(0xF59E0A).into(),
            danger: rgb(0xF7525F).into(),
            danger_soft: hsla(355.0 / 360.0, 0.91, 0.65, 0.10),
        }
    }
}

#[derive(Clone, Copy)]
struct ActiveWakuTheme(Theme);

impl Global for ActiveWakuTheme {}

/// Publish the resolved palette. [`Theme::current`] reads it back from the
/// global, which is how every view gets its colors.
fn set_active_theme(theme: Theme, cx: &mut App) {
    cx.set_global(ActiveWakuTheme(theme));
}

/// Resolve and publish the startup palette, before any window exists.
pub fn init(cx: &mut App) {
    let system_appearance = cx.window_appearance();
    let theme = if resolves_to_dark(ThemePreference::System, system_appearance) {
        Theme::dark()
    } else {
        Theme::light()
    };
    set_active_theme(theme, cx);
}

pub fn apply_theme_preference(preference: ThemePreference, window: &mut Window, cx: &mut App) {
    crate::platform::set_window_appearance(window, native_override(preference));
    let is_dark = resolves_to_dark(preference, cx.window_appearance());
    set_active_theme(
        if is_dark {
            Theme::dark()
        } else {
            Theme::light()
        },
        cx,
    );
    crate::platform::configure_sidebar_material(window, is_dark);
    window.refresh();
}

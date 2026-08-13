use gpui::{App, Global, Hsla, Rgba, Window, WindowAppearance, hsla, rgb, transparent_black};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemePreference {
    #[default]
    System,
    Light,
    Dark,
}

impl ThemePreference {
    pub const ALL: [Self; 3] = [Self::System, Self::Light, Self::Dark];

    pub fn label(self) -> String {
        match self {
            Self::System => tr!("settings.theme_system"),
            Self::Light => tr!("settings.theme_light"),
            Self::Dark => tr!("settings.theme_dark"),
        }
    }

    pub fn resolves_to_dark(self, system_appearance: WindowAppearance) -> bool {
        match self {
            Self::System => matches!(
                system_appearance,
                WindowAppearance::Dark | WindowAppearance::VibrantDark
            ),
            Self::Light => false,
            Self::Dark => true,
        }
    }

    fn native_override(self) -> Option<bool> {
        match self {
            Self::System => None,
            Self::Light => Some(false),
            Self::Dark => Some(true),
        }
    }
}

/// The authored surface of a palette. Everything a [`Theme`] needs is derived
/// from these fields, so a new palette is a handful of hexes rather than three
/// dozen hand-tuned tokens. `contrast` scales every wash and text tier: low
/// values keep secondary text and borders faint, high values push them toward
/// the ink.
#[derive(Clone, Copy)]
pub struct ThemeSeed {
    pub surface: u32,
    pub ink: u32,
    pub accent: u32,
    pub contrast: u8,
    pub added: u32,
    pub removed: u32,
    pub skill: u32,
    pub warning: u32,
}

#[derive(Clone, Copy)]
enum Palette {
    Seed(ThemeSeed),
    Authored(fn() -> Theme),
}

impl Palette {
    fn resolve(self, is_dark: bool) -> Theme {
        match self {
            Self::Seed(seed) => Theme::from_seed(seed, is_dark),
            Self::Authored(build) => build(),
        }
    }
}

pub struct ThemeDefinition {
    id: ThemeId,
    label: &'static str,
    light: Option<Palette>,
    dark: Option<Palette>,
}

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum ThemeId {
    #[default]
    Waku,
    Catppuccin,
    Dracula,
    Everforest,
    Github,
    Gruvbox,
    Monokai,
    NightOwl,
    Nord,
    One,
    RosePine,
    Solarized,
    TokyoNight,
    Vercel,
}

impl ThemeId {
    fn slug(self) -> &'static str {
        match self {
            Self::Waku => "waku",
            Self::Catppuccin => "catppuccin",
            Self::Dracula => "dracula",
            Self::Everforest => "everforest",
            Self::Github => "github",
            Self::Gruvbox => "gruvbox",
            Self::Monokai => "monokai",
            Self::NightOwl => "night-owl",
            Self::Nord => "nord",
            Self::One => "one",
            Self::RosePine => "rose-pine",
            Self::Solarized => "solarized",
            Self::TokyoNight => "tokyo-night",
            Self::Vercel => "vercel",
        }
    }

    fn from_slug(slug: &str) -> Option<Self> {
        THEMES
            .iter()
            .map(|definition| definition.id)
            .find(|id| id.slug() == slug)
    }

    fn definition(self) -> &'static ThemeDefinition {
        THEMES
            .iter()
            .find(|definition| definition.id == self)
            .unwrap_or(&THEMES[0])
    }

    pub fn label(self) -> &'static str {
        self.definition().label
    }

    fn palette(self, is_dark: bool) -> Option<Palette> {
        let definition = self.definition();
        if is_dark {
            definition.dark
        } else {
            definition.light
        }
    }

    pub fn supports(self, is_dark: bool) -> bool {
        self.palette(is_dark).is_some()
    }

    /// Palettes offered for one slot. A dark-only palette never appears in the
    /// light picker, so a chosen id always has a variant to resolve.
    pub fn all_for(is_dark: bool) -> Vec<Self> {
        THEMES
            .iter()
            .map(|definition| definition.id)
            .filter(|id| id.supports(is_dark))
            .collect()
    }

    pub fn resolve(self, is_dark: bool) -> Theme {
        self.palette(is_dark)
            .or_else(|| Self::default().palette(is_dark))
            .map(|palette| palette.resolve(is_dark))
            .unwrap_or_else(|| {
                if is_dark {
                    Theme::dark()
                } else {
                    Theme::light()
                }
            })
    }
}

impl Serialize for ThemeId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.slug())
    }
}

impl<'de> Deserialize<'de> for ThemeId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let slug = String::deserialize(deserializer)?;
        Ok(Self::from_slug(&slug).unwrap_or_default())
    }
}

const THEMES: &[ThemeDefinition] = &[
    ThemeDefinition {
        id: ThemeId::Waku,
        label: "Waku",
        light: Some(Palette::Authored(Theme::light)),
        dark: Some(Palette::Authored(Theme::dark)),
    },
    ThemeDefinition {
        id: ThemeId::Catppuccin,
        label: "Catppuccin",
        light: Some(Palette::Seed(ThemeSeed {
            surface: 0xEFF1F5,
            ink: 0x4C4F69,
            accent: 0x1E66D5,
            contrast: 46,
            added: 0x40A02B,
            removed: 0xD20F39,
            skill: 0x8839EF,
            warning: 0xDF8E1D,
        })),
        dark: Some(Palette::Seed(ThemeSeed {
            surface: 0x1E1E2E,
            ink: 0xCDD6F4,
            accent: 0x89B4FA,
            contrast: 58,
            added: 0xA6E3A1,
            removed: 0xF38BA8,
            skill: 0xCBA6F7,
            warning: 0xF9E2AF,
        })),
    },
    ThemeDefinition {
        id: ThemeId::Dracula,
        label: "Dracula",
        light: None,
        dark: Some(Palette::Seed(ThemeSeed {
            surface: 0x282A36,
            ink: 0xF8F8F2,
            accent: 0xBD93F9,
            contrast: 56,
            added: 0x50FA7B,
            removed: 0xFF5555,
            skill: 0xFF79C6,
            warning: 0xF1FA8C,
        })),
    },
    ThemeDefinition {
        id: ThemeId::Everforest,
        label: "Everforest",
        light: Some(Palette::Seed(ThemeSeed {
            surface: 0xFDF6E3,
            ink: 0x5C6A72,
            accent: 0x8DA101,
            contrast: 48,
            added: 0x8DA101,
            removed: 0xF85552,
            skill: 0xDF69BA,
            warning: 0xDFA000,
        })),
        dark: Some(Palette::Seed(ThemeSeed {
            surface: 0x2D353B,
            ink: 0xD3C6AA,
            accent: 0xA7C080,
            contrast: 58,
            added: 0xA7C080,
            removed: 0xE67E80,
            skill: 0xD699B6,
            warning: 0xDBBC7F,
        })),
    },
    ThemeDefinition {
        id: ThemeId::Github,
        label: "GitHub",
        light: Some(Palette::Seed(ThemeSeed {
            surface: 0xFFFFFF,
            ink: 0x1F2328,
            accent: 0x0969DA,
            contrast: 45,
            added: 0x1A7F37,
            removed: 0xCF222E,
            skill: 0x8250DF,
            warning: 0x9A6700,
        })),
        dark: Some(Palette::Seed(ThemeSeed {
            surface: 0x0D1117,
            ink: 0xE6EDF3,
            accent: 0x2F81F7,
            contrast: 60,
            added: 0x3FB950,
            removed: 0xF85149,
            skill: 0xA371F7,
            warning: 0xD29922,
        })),
    },
    ThemeDefinition {
        id: ThemeId::Gruvbox,
        label: "Gruvbox",
        light: Some(Palette::Seed(ThemeSeed {
            surface: 0xFBF1C7,
            ink: 0x3C3836,
            accent: 0x076678,
            contrast: 48,
            added: 0x79740E,
            removed: 0x9D0006,
            skill: 0x8F3F71,
            warning: 0xB57614,
        })),
        dark: Some(Palette::Seed(ThemeSeed {
            surface: 0x282828,
            ink: 0xEBDBB2,
            accent: 0x83A598,
            contrast: 56,
            added: 0xB8BB26,
            removed: 0xFB4934,
            skill: 0xD3869B,
            warning: 0xFABD2F,
        })),
    },
    ThemeDefinition {
        id: ThemeId::Monokai,
        label: "Monokai",
        light: None,
        dark: Some(Palette::Seed(ThemeSeed {
            surface: 0x272822,
            ink: 0xF8F8F2,
            accent: 0x66D9EF,
            contrast: 56,
            added: 0xA6E22E,
            removed: 0xF92672,
            skill: 0xAE81FF,
            warning: 0xE6DB74,
        })),
    },
    ThemeDefinition {
        id: ThemeId::NightOwl,
        label: "Night Owl",
        light: None,
        dark: Some(Palette::Seed(ThemeSeed {
            surface: 0x011627,
            ink: 0xD6DEEB,
            accent: 0x82AAFF,
            contrast: 58,
            added: 0xADDB67,
            removed: 0xEF5350,
            skill: 0xC792EA,
            warning: 0xECC48D,
        })),
    },
    ThemeDefinition {
        id: ThemeId::Nord,
        label: "Nord",
        light: None,
        dark: Some(Palette::Seed(ThemeSeed {
            surface: 0x2E3440,
            ink: 0xD8DEE9,
            accent: 0x88C0D0,
            contrast: 58,
            added: 0xA3BE8C,
            removed: 0xBF616A,
            skill: 0xB48EAD,
            warning: 0xEBCB8B,
        })),
    },
    ThemeDefinition {
        id: ThemeId::One,
        label: "One",
        light: Some(Palette::Seed(ThemeSeed {
            surface: 0xFAFAFA,
            ink: 0x383A42,
            accent: 0x4078F2,
            contrast: 46,
            added: 0x50A14F,
            removed: 0xE45649,
            skill: 0xA626A4,
            warning: 0xC18401,
        })),
        dark: Some(Palette::Seed(ThemeSeed {
            surface: 0x282C34,
            ink: 0xABB2BF,
            accent: 0x61AFEF,
            contrast: 60,
            added: 0x98C379,
            removed: 0xE06C75,
            skill: 0xC678DD,
            warning: 0xE5C07B,
        })),
    },
    ThemeDefinition {
        id: ThemeId::RosePine,
        label: "Rosé Pine",
        light: Some(Palette::Seed(ThemeSeed {
            surface: 0xFAF4ED,
            ink: 0x575279,
            accent: 0x907AA9,
            contrast: 48,
            added: 0x286983,
            removed: 0xB4637A,
            skill: 0x907AA9,
            warning: 0xEA9D34,
        })),
        dark: Some(Palette::Seed(ThemeSeed {
            surface: 0x232136,
            ink: 0xE0DEF4,
            accent: 0xC4A7E7,
            contrast: 58,
            added: 0x9CCFD8,
            removed: 0xEB6F92,
            skill: 0xC4A7E7,
            warning: 0xF6C177,
        })),
    },
    ThemeDefinition {
        id: ThemeId::Solarized,
        label: "Solarized",
        light: Some(Palette::Seed(ThemeSeed {
            surface: 0xFDF6E3,
            ink: 0x586E75,
            accent: 0x268BD2,
            contrast: 50,
            added: 0x859900,
            removed: 0xDC322F,
            skill: 0x6C71C4,
            warning: 0xB58900,
        })),
        dark: Some(Palette::Seed(ThemeSeed {
            surface: 0x002B36,
            ink: 0x93A1A1,
            accent: 0x268BD2,
            contrast: 62,
            added: 0x859900,
            removed: 0xDC322F,
            skill: 0x6C71C4,
            warning: 0xB58900,
        })),
    },
    ThemeDefinition {
        id: ThemeId::TokyoNight,
        label: "Tokyo Night",
        light: None,
        dark: Some(Palette::Seed(ThemeSeed {
            surface: 0x1A1B26,
            ink: 0xC0CAF5,
            accent: 0x7AA2F7,
            contrast: 58,
            added: 0x9ECE6A,
            removed: 0xF7768E,
            skill: 0xBB9AF7,
            warning: 0xE0AF68,
        })),
    },
    ThemeDefinition {
        id: ThemeId::Vercel,
        label: "Vercel",
        light: Some(Palette::Seed(ThemeSeed {
            surface: 0xFFFFFF,
            ink: 0x171717,
            accent: 0x0070F3,
            contrast: 44,
            added: 0x1A7F37,
            removed: 0xE5484D,
            skill: 0x8E4EC6,
            warning: 0xA35200,
        })),
        dark: Some(Palette::Seed(ThemeSeed {
            surface: 0x0A0A0A,
            ink: 0xEDEDED,
            accent: 0x0072F5,
            contrast: 60,
            added: 0x45A557,
            removed: 0xE5484D,
            skill: 0x8E4EC6,
            warning: 0xF5A623,
        })),
    },
];

fn hex(value: u32) -> Hsla {
    rgb(value).into()
}

fn with_alpha(color: Hsla, alpha: f32) -> Hsla {
    Hsla { a: alpha, ..color }
}

fn with_lightness(color: Hsla, lightness: f32) -> Hsla {
    Hsla {
        l: lightness.clamp(0.0, 1.0),
        ..color
    }
}

fn shift_lightness(color: Hsla, delta: f32) -> Hsla {
    with_lightness(color, color.l + delta)
}

fn mix(base: Hsla, other: Hsla, amount: f32) -> Hsla {
    let amount = amount.clamp(0.0, 1.0);
    let base = Rgba::from(base);
    let other = Rgba::from(other);
    Hsla::from(Rgba {
        r: base.r + (other.r - base.r) * amount,
        g: base.g + (other.g - base.g) * amount,
        b: base.b + (other.b - base.b) * amount,
        a: base.a + (other.a - base.a) * amount,
    })
}

fn luminance(color: Hsla) -> f32 {
    let color = Rgba::from(color);
    0.2126 * color.r + 0.7152 * color.g + 0.0722 * color.b
}

/// Waku's visual language, take two: neutral graphite surfaces in the spirit
/// of Cursor — color is reserved for meaning. On macOS the sidebar's semantic
/// tint is installed as a native layer above Sidebar vibrancy; keeping this
/// GPUI surface clear avoids incorrectly accumulating the alpha of nested Metal
/// backgrounds. Selected, hovered, and pressed rows remain a 6% neutral layer.
#[derive(Clone, Copy)]
pub struct Theme {
    pub is_dark: bool,
    pub canvas: Hsla,
    pub sidebar: Hsla,
    pub sidebar_drag_background: Hsla,
    /// The sidebar's own base color, painted as a native layer above Sidebar
    /// vibrancy. It has to live in the palette rather than in the platform
    /// layer: a themed canvas beside a fixed graphite sidebar reads as two
    /// different apps.
    pub sidebar_tint: Hsla,
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

    /// Brand coral. Logo, caret, live-activity pulses — nothing structural.
    pub accent: Hsla,
    /// Glyph laid directly on `accent`, flipped by the accent's own luminance
    /// so a pale palette does not print white on near-white.
    pub on_accent: Hsla,
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

    /// Expand a palette's authored fields into every token the app reads. The
    /// two branches mirror how depth reads in each mode: dark panels rise off
    /// the canvas by getting lighter, light panels recede by getting darker,
    /// and the composer is the one surface that goes the other way in light so
    /// it stays paper against a tinted canvas.
    pub fn from_seed(seed: ThemeSeed, is_dark: bool) -> Self {
        let seed_surface = hex(seed.surface);
        let ink = hex(seed.ink);
        let accent = hex(seed.accent);
        let contrast = f32::from(seed.contrast).clamp(0.0, 100.0) / 100.0;

        let (canvas, raised, composer, inset) = if is_dark {
            (
                seed_surface,
                shift_lightness(seed_surface, 0.035),
                shift_lightness(seed_surface, 0.027),
                shift_lightness(seed_surface, -0.020),
            )
        } else {
            (
                shift_lightness(seed_surface, -0.035),
                shift_lightness(seed_surface, -0.070),
                seed_surface,
                shift_lightness(seed_surface, -0.100),
            )
        };
        let terminal = if is_dark { inset } else { seed_surface };

        let wash = hsla(
            ink.h,
            ink.s.min(0.12),
            if is_dark { 0.90 } else { 0.12 },
            1.0,
        );

        let text_base = canvas;
        let warning = hex(seed.warning);

        Self {
            is_dark,
            canvas,
            sidebar: transparent_black(),
            sidebar_drag_background: shift_lightness(canvas, -0.012),
            sidebar_tint: shift_lightness(canvas, -0.012),
            sidebar_item_background: with_alpha(
                with_lightness(wash, if is_dark { 0.94 } else { 0.08 }),
                0.06,
            ),
            surface: canvas,
            raised,
            composer,
            inset,
            terminal,
            overlay: with_alpha(wash, 0.030 + contrast * 0.040),
            overlay_strong: with_alpha(wash, 0.055 + contrast * 0.070),

            border: with_alpha(wash, 0.040 + contrast * 0.060),
            border_strong: with_alpha(wash, 0.080 + contrast * 0.120),
            sidebar_border: if is_dark {
                shift_lightness(canvas, 0.058)
            } else {
                with_alpha(wash, 0.12)
            },

            text: ink,
            text_secondary: mix(text_base, ink, 0.60 + contrast * 0.17),
            text_tertiary: mix(text_base, ink, 0.40 + contrast * 0.21),
            text_ghost: mix(text_base, ink, 0.22 + contrast * 0.20),

            accent,
            on_accent: if luminance(accent) > 0.55 {
                with_lightness(accent, 0.10)
            } else {
                with_lightness(with_alpha(accent, 1.0), 0.98)
            },
            resize_handle: accent,
            gauge: accent,

            selection: with_alpha(accent, if is_dark { 0.42 } else { 0.28 }),
            code_text: hex(seed.skill),
            code_wash: with_alpha(wash, 0.060 + contrast * 0.040),

            inverse: if is_dark {
                with_lightness(ink, 0.91)
            } else {
                with_lightness(ink, 0.13)
            },
            on_inverse: if is_dark {
                with_lightness(canvas, 0.09)
            } else {
                with_lightness(canvas, 0.97)
            },

            warning,
            success: hex(seed.added),
            favorite: Hsla {
                s: (warning.s * 1.6).min(1.0),
                ..warning
            },
            danger: hex(seed.removed),
            danger_soft: with_alpha(hex(seed.removed), 0.10),
        }
    }

    pub fn dark() -> Self {
        Self {
            is_dark: true,
            canvas: rgb(0x1A1A1A).into(),
            sidebar: transparent_black(),
            sidebar_drag_background: rgb(0x181818).into(),
            sidebar_tint: rgb(0x181818).into(),
            sidebar_item_background: hsla(0.0, 0.0, 0.941, 0.06),
            surface: rgb(0x1A1A1A).into(),
            raised: rgb(0x232323).into(),
            composer: rgb(0x212121).into(),
            inset: rgb(0x151515).into(),
            terminal: rgb(0x151515).into(),
            overlay: hsla(220.0 / 360.0, 0.10, 0.90, 0.05),
            overlay_strong: hsla(220.0 / 360.0, 0.10, 0.90, 0.09),

            border: hsla(220.0 / 360.0, 0.10, 0.90, 0.07),
            border_strong: hsla(220.0 / 360.0, 0.10, 0.90, 0.14),
            sidebar_border: hsla(126.93 / 360.0, 0.000_000_1, 0.16077, 1.0),

            text: rgb(0xE2E2E2).into(),
            text_secondary: rgb(0xA3A3A3).into(),
            text_tertiary: rgb(0x7D7D7D).into(),
            text_ghost: rgb(0x575757).into(),

            accent: rgb(0xE2795B).into(),
            on_accent: rgb(0xFFFFFF).into(),
            resize_handle: rgb(0x3B82F6).into(),
            gauge: rgb(0x3B82F6).into(),

            selection: hsla(211.0 / 360.0, 1.0, 0.50, 0.55),
            code_text: rgb(0xE0A882).into(),
            code_wash: hsla(220.0 / 360.0, 0.10, 0.90, 0.08),

            inverse: rgb(0xE7E9EC).into(),
            on_inverse: rgb(0x17181C).into(),

            warning: rgb(0xE0B36A).into(),
            success: rgb(0x62C987).into(),
            favorite: rgb(0xEAB308).into(),
            danger: rgb(0xE2726A).into(),
            danger_soft: hsla(4.0 / 360.0, 0.55, 0.63, 0.10),
        }
    }

    pub fn light() -> Self {
        Self {
            is_dark: false,
            canvas: rgb(0xF6F5F6).into(),
            sidebar: transparent_black(),
            sidebar_drag_background: rgb(0xF3F3F3).into(),
            sidebar_tint: rgb(0xF3F3F3).into(),
            sidebar_item_background: hsla(0.0, 0.0, 0.078, 0.06),
            surface: rgb(0xF6F5F6).into(),
            raised: rgb(0xECECEC).into(),
            composer: rgb(0xFFFFFF).into(),
            inset: rgb(0xE6E6E6).into(),
            terminal: rgb(0xFFFFFF).into(),
            overlay: hsla(220.0 / 360.0, 0.10, 0.12, 0.05),
            overlay_strong: hsla(220.0 / 360.0, 0.10, 0.12, 0.09),

            border: hsla(220.0 / 360.0, 0.10, 0.12, 0.08),
            border_strong: hsla(220.0 / 360.0, 0.10, 0.12, 0.15),
            sidebar_border: hsla(0.0, 0.0, 0.078, 0.12),

            text: rgb(0x242424).into(),
            text_secondary: rgb(0x666666).into(),
            text_tertiary: rgb(0x858585).into(),
            text_ghost: rgb(0xA4A4A4).into(),

            accent: rgb(0xC85F44).into(),
            on_accent: rgb(0xFFFFFF).into(),
            resize_handle: rgb(0x2563EB).into(),
            gauge: rgb(0x2563EB).into(),

            selection: hsla(211.0 / 360.0, 1.0, 0.50, 0.35),
            code_text: rgb(0x9A5528).into(),
            code_wash: hsla(220.0 / 360.0, 0.10, 0.12, 0.07),

            inverse: rgb(0x202227).into(),
            on_inverse: rgb(0xF8F8F9).into(),

            warning: rgb(0xA66B20).into(),
            success: rgb(0x2F8F52).into(),
            favorite: rgb(0xCA8A04).into(),
            danger: rgb(0xC64A42).into(),
            danger_soft: hsla(4.0 / 360.0, 0.55, 0.52, 0.10),
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
    let is_dark = ThemePreference::System.resolves_to_dark(system_appearance);
    set_active_theme(ThemeId::default().resolve(is_dark), cx);
}

pub fn apply_theme_preference(
    preference: ThemePreference,
    light_theme: ThemeId,
    dark_theme: ThemeId,
    window: &mut Window,
    cx: &mut App,
) {
    crate::platform::set_window_appearance(window, preference.native_override());
    let is_dark = preference.resolves_to_dark(cx.window_appearance());
    let selected = if is_dark { dark_theme } else { light_theme };
    let theme = selected.resolve(is_dark);
    set_active_theme(theme, cx);
    crate::platform::configure_sidebar_material(window, is_dark, theme.sidebar_tint);
    window.refresh();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_theme_offers_at_least_one_variant() {
        for definition in THEMES {
            assert!(
                definition.light.is_some() || definition.dark.is_some(),
                "{} registers no variant",
                definition.label
            );
        }
    }

    #[test]
    fn slugs_round_trip_and_are_unique() {
        for definition in THEMES {
            assert_eq!(
                ThemeId::from_slug(definition.id.slug()),
                Some(definition.id)
            );
        }
        let mut slugs: Vec<_> = THEMES
            .iter()
            .map(|definition| definition.id.slug())
            .collect();
        slugs.sort_unstable();
        let count = slugs.len();
        slugs.dedup();
        assert_eq!(slugs.len(), count);
    }

    #[test]
    fn unknown_slug_falls_back_to_default() {
        let id: ThemeId = serde_json::from_str("\"not-a-theme\"").unwrap();
        assert_eq!(id, ThemeId::default());
        assert_eq!(
            serde_json::to_string(&ThemeId::TokyoNight).unwrap(),
            "\"tokyo-night\""
        );
    }

    #[test]
    fn pickers_only_offer_supported_variants() {
        assert!(!ThemeId::all_for(false).contains(&ThemeId::Dracula));
        assert!(ThemeId::all_for(true).contains(&ThemeId::Dracula));
        for is_dark in [false, true] {
            for id in ThemeId::all_for(is_dark) {
                assert_eq!(id.resolve(is_dark).is_dark, is_dark);
            }
        }
    }

    #[test]
    fn derived_surfaces_separate_from_the_canvas() {
        for is_dark in [false, true] {
            for id in ThemeId::all_for(is_dark) {
                let theme = id.resolve(is_dark);
                assert_ne!(
                    theme.raised,
                    theme.canvas,
                    "{} {} has a flat raised surface",
                    id.slug(),
                    is_dark
                );
                assert!(
                    (luminance(theme.text) - luminance(theme.canvas)).abs() > 0.25,
                    "{} {} text is too close to its canvas",
                    id.slug(),
                    is_dark
                );
            }
        }
    }
}

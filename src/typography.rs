use gpui::{ListState, Pixels, Rems, Styled, Window, px, rems};
use serde::{Deserialize, Deserializer, Serialize};

/// GPUI's default root-em size. Text authored with [`rems_from_px`] keeps its
/// current appearance at 100% while still following the user's text scale.
pub const BASE_REM_SIZE_IN_PX: f32 = 16.0;

/// User-selectable scale for text and text-aligned icons rendered by Waku.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FontSizePreference {
    Small,
    #[default]
    Default,
    Large,
    ExtraLarge,
}

impl<'de> Deserialize<'de> for FontSizePreference {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        // `settings.json` is user-editable. An unknown future preset, typo, or
        // wrong JSON type must not make the entire application state fail to
        // load; only this preference falls back to its safe default.
        let value = serde_json::Value::deserialize(deserializer)?;
        Ok(match value.as_str() {
            Some("small") => Self::Small,
            Some("default") => Self::Default,
            Some("large") => Self::Large,
            Some("extra_large") => Self::ExtraLarge,
            _ => Self::default(),
        })
    }
}

impl FontSizePreference {
    pub const ALL: [Self; 4] = [Self::Small, Self::Default, Self::Large, Self::ExtraLarge];

    pub const fn scale(self) -> f32 {
        match self {
            Self::Small => 0.9,
            Self::Default => 1.0,
            Self::Large => 1.15,
            Self::ExtraLarge => 1.3,
        }
    }

    pub fn label(self) -> String {
        match self {
            Self::Small => tr!("settings.font_size_small"),
            Self::Default => tr!("settings.font_size_default"),
            Self::Large => tr!("settings.font_size_large"),
            Self::ExtraLarge => tr!("settings.font_size_extra_large"),
        }
    }

    pub fn rem_size(self) -> Pixels {
        px(BASE_REM_SIZE_IN_PX * self.scale())
    }
}

/// Convert a pixel value authored at Waku's default text scale into rems.
///
/// This mirrors Zed's `rems_from_px` convention: callers retain readable
/// design values while GPUI resolves them against `Window::rem_size` during
/// layout and text measurement.
#[inline(always)]
pub fn rems_from_px(pixels: f32) -> Rems {
    rems(pixels / BASE_REM_SIZE_IN_PX)
}

/// Waku's authored-pixel text-size API.
///
/// GPUI already owns a `Styled::text_size` method, so Rust cannot safely
/// override it with a same-named extension method. This deliberately distinct
/// entry point keeps call sites in familiar design pixels while ensuring every
/// explicit font size follows `Window::rem_size`.
pub trait TextSizeExt: Styled {
    #[inline(always)]
    fn text_size_px(self, pixels: f32) -> Self {
        Styled::text_size(self, rems_from_px(pixels))
    }
}

impl<T: Styled> TextSizeExt for T {}

pub fn apply_font_size_preference(preference: FontSizePreference, window: &mut Window) {
    window.set_rem_size(preference.rem_size());
    window.invalidate_character_coordinates();
    window.refresh();
}

/// Clear a virtual list's stale size hints after a global scale change while
/// retaining its logical item anchor. Keeping the old hints would make a long
/// list's scrollbar converge one newly visited row at a time.
pub fn reset_scaled_list_measurements(
    list: &ListState,
    item_count: usize,
    scale_ratio: f32,
    bottom_aligned_when_empty: bool,
) {
    let previous_count = list.item_count();
    let mut scroll_top = list.logical_scroll_top();
    let was_at_end = previous_count > 0 && scroll_top.item_ix >= previous_count;

    // A full-range splice produces unmeasured rows with no old-size hints but
    // does not eagerly render the entire collection.
    list.splice(0..previous_count, item_count);

    if was_at_end || (previous_count == 0 && bottom_aligned_when_empty) {
        list.scroll_to_end();
    } else {
        scroll_top.offset_in_item *= scale_ratio;
        list.scroll_to(scroll_top);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_scale_preserves_authored_pixel_sizes() {
        assert_eq!(
            rems_from_px(13.5).to_pixels(FontSizePreference::Default.rem_size()),
            px(13.5)
        );
    }

    #[test]
    fn font_size_presets_are_ordered_and_scale_text() {
        let sizes = FontSizePreference::ALL
            .map(|preference| rems_from_px(13.5).to_pixels(preference.rem_size()));

        assert!(sizes.windows(2).all(|pair| pair[0] < pair[1]));
        assert_eq!(sizes[2], px(15.525));
    }

    #[test]
    fn unknown_or_malformed_preferences_fall_back_without_failing_settings() {
        for value in [
            serde_json::json!("future_size"),
            serde_json::json!(150),
            serde_json::Value::Null,
        ] {
            assert_eq!(
                serde_json::from_value::<FontSizePreference>(value).unwrap(),
                FontSizePreference::Default
            );
        }
    }

    #[test]
    fn resetting_list_measurements_preserves_the_scaled_logical_anchor() {
        use gpui::{ListAlignment, ListOffset};

        let list = ListState::new(5, ListAlignment::Top, px(100.0));
        list.scroll_to(ListOffset {
            item_ix: 2,
            offset_in_item: px(4.0),
        });

        reset_scaled_list_measurements(&list, 5, 1.3, false);

        assert_eq!(list.item_count(), 5);
        let scroll_top = list.logical_scroll_top();
        assert_eq!(scroll_top.item_ix, 2);
        assert_eq!(scroll_top.offset_in_item, px(5.2));
    }

    #[test]
    fn an_empty_top_aligned_list_stays_at_its_start() {
        use gpui::ListAlignment;

        let list = ListState::new(0, ListAlignment::Top, px(100.0));
        reset_scaled_list_measurements(&list, 4, 1.3, false);

        assert_eq!(list.logical_scroll_top().item_ix, 0);
    }
}

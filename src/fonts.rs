//! Installed monospaced families for the Appearance code-font picker.
//!
//! Walking the OS catalog and measuring advances is too expensive for a
//! frame, so the list is built once on a background thread after bundled
//! fonts are registered. Render reads only the cache.

use std::sync::Arc;

use gpui::{App, Font, Global, Pixels, SharedString, TextSystem, font, px};
use parking_lot::RwLock;

use crate::persistence::DEFAULT_CODE_FONT_FAMILY;

struct FontFamilyCacheState {
    families: Option<Vec<SharedString>>,
}

#[derive(Clone)]
struct FontFamilyCache {
    state: Arc<RwLock<FontFamilyCacheState>>,
}

impl Global for FontFamilyCache {}

impl FontFamilyCache {
    fn new() -> Self {
        Self {
            state: Arc::new(RwLock::new(FontFamilyCacheState { families: None })),
        }
    }
}

/// Start the one-shot catalog walk. Safe to call once at process start;
/// later calls are no-ops once a load is in flight or finished.
pub fn init(cx: &mut App) {
    if cx.has_global::<FontFamilyCache>() {
        return;
    }
    let cache = FontFamilyCache::new();
    cx.set_global(cache.clone());
    let text_system = cx.text_system().clone();
    cx.spawn(async move |cx| {
        let families = cx
            .background_executor()
            .spawn(async move { collect_monospace_families(&text_system) })
            .await;
        let _ = cx.update(|cx| {
            cache.state.write().families = Some(families);
            cx.refresh_windows();
        });
    })
    .detach();
}

/// Monospaced families the picker can offer. Empty until the background
/// catalog walk finishes; the trigger still shows the saved family.
pub fn monospace_families(cx: &App) -> Vec<SharedString> {
    cx.try_global::<FontFamilyCache>()
        .and_then(|cache| cache.state.read().families.clone())
        .unwrap_or_default()
}

/// Families shown in the picker: the bundled default first, then installed
/// monospaced faces, then the current choice if it is not already listed so
/// a hand-edited `app.json` value remains selectable.
pub fn picker_families(current: &str, cx: &App) -> Vec<SharedString> {
    let mut families = Vec::new();
    let mut push = |name: &str| {
        if name.is_empty() {
            return;
        }
        if families
            .iter()
            .any(|existing: &SharedString| existing.as_ref() == name)
        {
            return;
        }
        families.push(SharedString::from(name.to_owned()));
    };
    push(DEFAULT_CODE_FONT_FAMILY);
    for family in monospace_families(cx) {
        push(family.as_ref());
    }
    push(current);
    families
}

fn collect_monospace_families(text_system: &TextSystem) -> Vec<SharedString> {
    let mut names: Vec<String> = text_system
        .all_font_names()
        .into_iter()
        .filter(|name| is_candidate_code_font(name) && is_monospaced(text_system, name))
        .collect();
    names.sort_by(|a, b| a.to_ascii_lowercase().cmp(&b.to_ascii_lowercase()));
    names.dedup();
    names.into_iter().map(SharedString::from).collect()
}

fn is_candidate_code_font(name: &str) -> bool {
    if name.is_empty() || name.starts_with('.') {
        return false;
    }
    let lower = name.to_ascii_lowercase();
    !lower.contains("nerd font")
        && !lower.contains("awesome")
        && !lower.contains("emoji")
        && !lower.contains("symbol")
}

fn is_monospaced(text_system: &TextSystem, family: &str) -> bool {
    let font: Font = font(family);
    let font_id = text_system.resolve_font(&font);
    let size: Pixels = px(12.0);
    let Ok(i) = text_system.advance(font_id, size, 'i') else {
        return false;
    };
    let Ok(m) = text_system.advance(font_id, size, 'm') else {
        return false;
    };
    (i.width - m.width).abs() < px(0.05)
}

#[cfg(test)]
mod tests {
    use super::is_candidate_code_font;

    #[test]
    fn internal_and_symbol_faces_are_not_offered() {
        assert!(!is_candidate_code_font(".SystemUIFont"));
        assert!(!is_candidate_code_font("Symbols Nerd Font Mono"));
        assert!(!is_candidate_code_font("Apple Color Emoji"));
        assert!(is_candidate_code_font("JetBrains Mono"));
        assert!(is_candidate_code_font("Menlo"));
    }
}

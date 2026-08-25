//! Resolved font families.
//!
//! Every render path reads [`sans`] / [`mono`], which return a plain
//! `&'static str` from a lock-protected static — no `cx`, no allocation.
//! Values are installed once at window startup and again whenever the
//! setting changes; each install leaks at most two short strings, which is
//! bounded by how often a user changes their font in a session.
//!
//! Custom families are validated against the platform font list at choice
//! time (see the settings UI), so the install path trusts persisted values.
//! A hand-edited settings file naming a missing family falls back through
//! GPUI's per-platform resolution; it cannot crash rendering.

use std::sync::{atomic::AtomicPtr, atomic::Ordering, OnceLock};

use gpui::App;
pub use waku_client::persistence::{DEFAULT_MONO_FAMILY, DEFAULT_SANS_FAMILY};

#[derive(Clone, Copy)]
struct Families {
    sans: &'static str,
    mono: &'static str,
}

const DEFAULT_FAMILIES: Families = Families {
    sans: DEFAULT_SANS_FAMILY,
    mono: DEFAULT_MONO_FAMILY,
};

static FAMILIES: AtomicPtr<Families> = AtomicPtr::new(&DEFAULT_FAMILIES as *const Families as *mut Families);

fn leak(name: &str) -> &'static str {
    Box::leak(name.to_string().into_boxed_str())
}

/// Install the resolved families from persisted settings. Called when a
/// window opens and whenever either setting changes.
pub fn install(ui_font_family: Option<&str>, mono_font_family: Option<&str>) {
    let resolve = |candidate: Option<&str>, fallback: &'static str| match candidate {
        Some(family) if !family.trim().is_empty() => leak(family.trim()),
        _ => fallback,
    };
    let families = Box::new(Families {
        sans: resolve(ui_font_family, DEFAULT_SANS_FAMILY),
        mono: resolve(mono_font_family, DEFAULT_MONO_FAMILY),
    });
    let ptr = Box::leak(families) as *mut Families;
    FAMILIES.store(ptr, Ordering::Release);
}

/// The resolved UI sans family. Defaults to the platform system font.
pub fn sans() -> &'static str {
    // SAFETY: FAMILIES always points to a leaked Box<Families>, either
    // DEFAULT_FAMILIES (static) or a heap allocation from install().
    // Ordering::Acquire synchronizes with the Release store in install().
    unsafe { &*FAMILIES.load(Ordering::Acquire) }.sans
}

/// The resolved monospace family for code surfaces.
pub fn mono() -> &'static str {
    // SAFETY: FAMILIES always points to a leaked Box<Families>, either
    // DEFAULT_FAMILIES (static) or a heap allocation from install().
    // Ordering::Acquire synchronizes with the Release store in install().
    unsafe { &*FAMILIES.load(Ordering::Acquire) }.mono
}

/// Every installed system font family, sorted. Enumerated once per process
/// on first use — a one-shot user action such as opening a settings menu —
/// never on a render path.
pub fn available_font_names(cx: &App) -> &'static [String] {
    static AVAILABLE: OnceLock<Vec<String>> = OnceLock::new();
    AVAILABLE.get_or_init(|| {
        let mut names = cx.text_system().all_font_names();
        names.retain(|name| !name.starts_with('.'));
        names.sort_unstable();
        names.dedup();
        names
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Serialize tests that modify global FAMILIES to prevent races.
    // Without this lock, defaults_match_platform_fallbacks might read
    // values installed by install_overrides_and_blank_resets when
    // tests run concurrently, causing flaky failures.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn defaults_match_platform_fallbacks() {
        let _guard = TEST_LOCK.lock().unwrap();
        // Reset to known state in case a previous test left modifications
        install(None, None);
        assert_eq!(sans(), DEFAULT_SANS_FAMILY);
        assert_eq!(mono(), DEFAULT_MONO_FAMILY);
    }

    #[test]
    fn install_overrides_and_blank_resets() {
        let _guard = TEST_LOCK.lock().unwrap();

        install(Some("Fira Code"), None);
        assert_eq!(mono(), DEFAULT_MONO_FAMILY);

        install(None, Some("IBM Plex Mono"));
        assert_eq!(sans(), DEFAULT_SANS_FAMILY);
        assert_eq!(mono(), "IBM Plex Mono");

        install(Some("  "), Some(""));
        assert_eq!(sans(), DEFAULT_SANS_FAMILY);
        assert_eq!(mono(), DEFAULT_MONO_FAMILY);

        install(None, None);
    }

    #[test]
    fn concurrent_reads_do_not_block() {
        // Regression test for P1: verify sans() and mono() can be called
        // from multiple threads without blocking. The implementation must
        // not use locks in the read path.
        let _guard = TEST_LOCK.lock().unwrap();
        install(Some("Test Sans"), Some("Test Mono"));

        let handles: Vec<_> = (0..10)
            .map(|_| {
                std::thread::spawn(|| {
                    for _ in 0..1000 {
                        let _ = sans();
                        let _ = mono();
                    }
                })
            })
            .collect();

        for handle in handles {
            handle.join().unwrap();
        }

        // Clean up
        install(None, None);
    }
}

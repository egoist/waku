//! Shared locale selection and translation access.

use serde::{Deserialize, Serialize};

/// The language preference Waku persists. `System` resolves to one of the
/// locales Waku deliberately ships today.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AppLanguage {
    System,
    English,
    SimplifiedChinese,
    Japanese,
}

impl AppLanguage {
    pub const ALL: [Self; 4] = [
        Self::System,
        Self::English,
        Self::SimplifiedChinese,
        Self::Japanese,
    ];

    pub fn locale(self) -> &'static str {
        match self.resolved() {
            Self::System => unreachable!("system language always resolves to a shipped locale"),
            Self::English => "en",
            Self::SimplifiedChinese => "zh-CN",
            Self::Japanese => "ja",
        }
    }

    /// Explicit language names are autonyms so the selector remains
    /// understandable even when the current locale is unfamiliar.
    pub fn label(self) -> String {
        match self {
            Self::System => translate("language.system"),
            Self::English => "English".to_owned(),
            Self::SimplifiedChinese => "简体中文".to_owned(),
            Self::Japanese => "日本語".to_owned(),
        }
    }

    pub fn resolved(self) -> Self {
        match self {
            Self::System => Self::from_system(),
            explicit => explicit,
        }
    }

    fn from_system() -> Self {
        Self::from_locale_id(&system_locale())
    }

    fn from_locale_id(locale: &str) -> Self {
        let locale = locale.replace('_', "-").to_ascii_lowercase();
        if locale == "zh-cn" || locale == "zh-sg" || locale.starts_with("zh-hans") {
            Self::SimplifiedChinese
        } else if locale == "ja" || locale.starts_with("ja-") {
            Self::Japanese
        } else {
            Self::English
        }
    }
}

impl Default for AppLanguage {
    fn default() -> Self {
        Self::System
    }
}

pub fn set_language(language: AppLanguage) {
    rust_i18n::set_locale(language.locale());
}

pub fn translate(key: &str) -> String {
    rust_i18n::t!(key).into_owned()
}

pub fn uses_east_asian_date_format() -> bool {
    locale_uses_east_asian_date_format(&rust_i18n::locale())
}

fn locale_uses_east_asian_date_format(locale: &str) -> bool {
    matches!(locale, "zh-CN" | "ja")
}

#[cfg(target_os = "macos")]
fn system_locale() -> String {
    use objc2_foundation::NSLocale;

    NSLocale::preferredLanguages()
        .firstObject()
        .map(|locale| locale.to_string())
        .unwrap_or_else(|| "en".to_owned())
}

#[cfg(not(target_os = "macos"))]
fn system_locale() -> String {
    std::env::var("LC_ALL")
        .or_else(|_| std::env::var("LC_MESSAGES"))
        .or_else(|_| std::env::var("LANG"))
        .unwrap_or_else(|_| "en".to_owned())
        .split('.')
        .next()
        .unwrap_or("en")
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};

    const EN_CATALOG: &str = include_str!("../../../locales/app.yml");
    const ZH_CATALOG: &str = include_str!("../../../locales/zh-CN.yml");
    const JA_CATALOG: &str = include_str!("../../../locales/ja.yml");

    #[test]
    fn language_locale_ids_are_supported() {
        assert_eq!(AppLanguage::English.locale(), "en");
        assert_eq!(AppLanguage::SimplifiedChinese.locale(), "zh-CN");
        assert_eq!(AppLanguage::Japanese.locale(), "ja");
        let locales = rust_i18n::available_locales!();
        assert_eq!(locales.len(), 3);
        assert!(locales.iter().any(|locale| locale.as_ref() == "en"));
        assert!(locales.iter().any(|locale| locale.as_ref() == "zh-CN"));
        assert!(locales.iter().any(|locale| locale.as_ref() == "ja"));
    }

    #[test]
    fn language_names_are_autonyms() {
        assert_eq!(AppLanguage::English.label(), "English");
        assert_eq!(AppLanguage::SimplifiedChinese.label(), "简体中文");
        assert_eq!(AppLanguage::Japanese.label(), "日本語");
    }

    #[test]
    fn system_is_the_default_persisted_preference_and_resolves_to_a_shipped_locale() {
        assert_eq!(AppLanguage::default(), AppLanguage::System);
        assert_eq!(
            serde_json::to_string(&AppLanguage::System).unwrap(),
            r#""system""#
        );
        assert!(matches!(
            AppLanguage::System.locale(),
            "en" | "zh-CN" | "ja"
        ));
    }

    #[test]
    fn japanese_system_locales_are_detected() {
        assert_eq!(AppLanguage::from_locale_id("ja"), AppLanguage::Japanese);
        assert_eq!(AppLanguage::from_locale_id("ja_JP"), AppLanguage::Japanese);
    }

    #[test]
    fn japanese_and_simplified_chinese_use_east_asian_dates() {
        assert!(locale_uses_east_asian_date_format("ja"));
        assert!(locale_uses_east_asian_date_format("zh-CN"));
        assert!(!locale_uses_east_asian_date_format("en"));
    }

    #[test]
    fn simplified_chinese_system_locales_are_detected_without_enabling_traditional_chinese() {
        assert_eq!(
            AppLanguage::from_locale_id("zh-Hans-CN"),
            AppLanguage::SimplifiedChinese
        );
        assert_eq!(
            AppLanguage::from_locale_id("zh_SG"),
            AppLanguage::SimplifiedChinese
        );
        assert_eq!(
            AppLanguage::from_locale_id("zh-Hant-TW"),
            AppLanguage::English
        );
    }

    #[test]
    fn translations_are_complete_and_interpolate_naturally() {
        assert_eq!(&*rust_i18n::t!("settings.daemon", locale = "en"), "Daemon");
        assert_eq!(
            &*rust_i18n::t!("daemon.expose_title", locale = "en"),
            "Expose managed daemon"
        );
        assert_eq!(
            &*rust_i18n::t!("settings.general", locale = "zh-CN"),
            "通用"
        );
        assert_eq!(
            &*rust_i18n::t!(
                "computer_use.allow_control",
                locale = "zh-CN",
                app = "Finder"
            ),
            "允许 Waku 控制“Finder”吗？"
        );
        assert_eq!(
            &*rust_i18n::t!("session.rewound", locale = "zh-CN", turn = 3),
            "已回退到第 3 轮任务之前"
        );
        assert_eq!(&*rust_i18n::t!("settings.general", locale = "ja"), "一般");
        assert_eq!(
            &*rust_i18n::t!("computer_use.allow_control", locale = "ja", app = "Finder"),
            "Waku に「Finder」の操作を許可しますか？"
        );
        assert_eq!(
            &*rust_i18n::t!("session.rewound", locale = "ja", turn = 3),
            "タスクをターン 3 の前まで巻き戻しました"
        );
    }

    #[test]
    fn task_creation_copy_uses_task_terminology() {
        assert_eq!(&*rust_i18n::t!("menu.new_task", locale = "en"), "New Task");
        assert_eq!(
            &*rust_i18n::t!("command_palette.new_task", locale = "en"),
            "New task"
        );
        assert_eq!(
            &*rust_i18n::t!("providers.disabled_for_new_tasks", locale = "en"),
            "Disabled for new tasks"
        );
    }

    /// A locale that is missing a message shows the English one instead, and
    /// nobody notices: the translation registry falls back silently. The
    /// catalogs are therefore kept in step by a test rather than by whoever
    /// remembers to look (#163).
    #[test]
    fn every_locale_defines_the_same_messages_as_english() {
        let english = catalog_messages(EN_CATALOG, "en");
        assert!(!english.is_empty());
        for (locale, source) in [("ja", JA_CATALOG), ("zh-CN", ZH_CATALOG)] {
            let messages = catalog_messages(source, locale);
            let missing: Vec<&String> = english
                .keys()
                .filter(|key| !messages.contains_key(*key))
                .collect();
            let extra: Vec<&String> = messages
                .keys()
                .filter(|key| !english.contains_key(*key))
                .collect();
            assert!(
                missing.is_empty() && extra.is_empty(),
                "the {locale} catalog drifted: missing {missing:?}, unknown {extra:?}"
            );
        }
    }

    /// A translation that drops a placeholder shows the raw `%{name}` to the
    /// reader, so the placeholders of a locale have to be the ones of the
    /// message they translate.
    #[test]
    fn translations_keep_the_placeholders_of_their_english_message() {
        let english = catalog_messages(EN_CATALOG, "en");
        for (locale, source) in [("ja", JA_CATALOG), ("zh-CN", ZH_CATALOG)] {
            let messages = catalog_messages(source, locale);
            for (key, english_value) in &english {
                let Some(value) = messages.get(key) else {
                    continue;
                };
                assert_eq!(
                    placeholders(value),
                    placeholders(english_value),
                    "the {locale} translation of {key} does not use the same placeholders"
                );
            }
        }
    }

    /// The messages one catalog defines for a locale.
    ///
    /// The catalogs are flat `key: value` maps, except the English one, which
    /// nests every value under its locale so a later locale can share the
    /// file. Both shapes are read here the way the translation registry reads
    /// them, and the metadata key rust_i18n ignores is left out.
    fn catalog_messages(source: &str, locale: &str) -> BTreeMap<String, String> {
        let mut messages = BTreeMap::new();
        let mut pending: Option<String> = None;
        for line in source.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            let indented = line.len() != trimmed.len();
            let Some((key, value)) = trimmed.split_once(':') else {
                continue;
            };
            let key = key.trim();
            let value = value.trim();
            if indented {
                // A nested value belongs to the message that opened the block.
                let Some(parent) = pending.as_ref() else {
                    continue;
                };
                if key == locale {
                    messages.insert(parent.clone(), unquote(value));
                    pending = None;
                }
                continue;
            }
            if key.starts_with('_') {
                continue;
            }
            pending = None;
            if value.is_empty() {
                pending = Some(key.to_owned());
                continue;
            }
            messages.insert(key.to_owned(), unquote(value));
        }
        messages
    }

    /// The value a catalog writes, without the quotes a value may carry.
    fn unquote(value: &str) -> String {
        if !value.starts_with('"') {
            return value.to_owned();
        }
        serde_json::from_str::<String>(value).unwrap_or_else(|_| value.to_owned())
    }

    /// The placeholders one message interpolates.
    fn placeholders(value: &str) -> BTreeSet<&str> {
        let mut found = BTreeSet::new();
        let mut rest = value;
        while let Some(start) = rest.find("%{") {
            let after = &rest[start + 2..];
            let Some(end) = after.find('}') else {
                break;
            };
            found.insert(&after[..end]);
            rest = &after[end + 1..];
        }
        found
    }
}

//! Conversation typography preference persisted by desktop clients.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConversationTextSize {
    Small,
    #[default]
    Default,
    Large,
}

impl ConversationTextSize {
    pub const ALL: [Self; 3] = [Self::Small, Self::Default, Self::Large];

    pub fn label(self) -> String {
        match self {
            Self::Small => crate::i18n::translate("settings.text_size_small"),
            Self::Default => crate::i18n::translate("settings.text_size_default"),
            Self::Large => crate::i18n::translate("settings.text_size_large"),
        }
    }

    pub const fn scale(self) -> f32 {
        match self {
            Self::Small => 0.92,
            Self::Default => 1.0,
            Self::Large => 1.12,
        }
    }
}

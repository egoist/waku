use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize, TS)]
pub enum CommandScope {
    Project,
    User,
    Skill,
    Builtin,
}

impl CommandScope {
    pub fn label(self) -> String {
        match self {
            Self::Project => tr!("command_scope.project"),
            Self::User => tr!("command_scope.user"),
            Self::Skill => tr!("command_scope.skill"),
            Self::Builtin => tr!("command_scope.builtin"),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
pub struct SlashCommand {
    pub name: String,
    pub description: String,
    pub scope: CommandScope,
    pub argument_hint: Option<String>,
    pub template: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
pub struct FileEntry {
    pub path: String,
    pub is_dir: bool,
}

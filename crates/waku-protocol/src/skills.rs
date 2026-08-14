use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::model::ProviderKind;

pub const SKILL_FILE: &str = "SKILL.md";
pub const DISABLED_SKILL_FILE: &str = "SKILL.md.disabled";

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum SkillSource {
    Shared,
    Provider(ProviderKind),
}

impl SkillSource {
    pub fn label(self) -> String {
        match self {
            Self::Shared => tr!("skills.source_shared"),
            Self::Provider(provider) => provider.short_name().to_owned(),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum SkillScope {
    User,
    Project,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct SkillLocation {
    pub source: SkillSource,
    pub scope: SkillScope,
    #[ts(type = "string")]
    pub root: PathBuf,
    pub project: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct SkillInstall {
    pub source: SkillSource,
    #[ts(type = "string")]
    pub dir: PathBuf,
    #[ts(type = "string")]
    pub skill_file: PathBuf,
    pub enabled: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct SkillEntry {
    pub name: String,
    pub description: String,
    pub scope: SkillScope,
    pub project: Option<String>,
    pub installs: Vec<SkillInstall>,
    pub enabled: bool,
    pub allowed_tools: Option<String>,
    pub body: String,
    pub supporting_files: usize,
    pub total_bytes: u64,
    pub modified_at: Option<u64>,
    pub duplicates: usize,
    pub row_key: u64,
}

impl SkillEntry {
    pub fn primary(&self) -> &SkillInstall {
        &self.installs[0]
    }

    pub fn sources_label(&self) -> String {
        self.installs
            .iter()
            .map(|install| install.source.label())
            .collect::<Vec<_>>()
            .join(" · ")
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct SkillsCatalog {
    pub skills: Vec<SkillEntry>,
}

impl SkillsCatalog {
    pub fn disabled_count(&self) -> usize {
        self.skills.iter().filter(|skill| !skill.enabled).count()
    }
}

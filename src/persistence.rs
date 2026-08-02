use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::computer_use::ComputerAppGrant;
use crate::identity::DATA_DIRECTORY_NAME;
use crate::model::{AgentSession, FavoriteModel, Project, ProviderKind};
use crate::theme::ThemePreference;

const STATE_VERSION: u32 = 4;
const OLDEST_SUPPORTED_STATE_VERSION: u32 = 1;

pub const DEFAULT_SIDEBAR_WIDTH: f32 = 252.0;
pub const DEFAULT_RIGHT_PANEL_WIDTH: f32 = 460.0;

fn default_panel_visibility() -> bool {
    true
}

fn default_computer_use_enabled() -> bool {
    true
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PersistedState {
    pub version: u32,
    pub projects: Vec<Project>,
    pub sessions: Vec<AgentSession>,
    pub selected_project: Option<Uuid>,
    pub selected_session: Option<Uuid>,
    pub last_provider: ProviderKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_reasoning_effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_service_tier: Option<String>,
    #[serde(default)]
    pub favorite_models: Vec<FavoriteModel>,
    #[serde(default)]
    pub theme: ThemePreference,
    #[serde(default = "default_panel_visibility")]
    pub sidebar_visible: bool,
    #[serde(default = "default_panel_visibility")]
    pub right_panel_visible: bool,
    #[serde(default = "default_sidebar_width")]
    pub sidebar_width: f32,
    #[serde(default = "default_right_panel_width")]
    pub right_panel_width: f32,
    #[serde(default = "default_computer_use_enabled")]
    pub computer_use_enabled: bool,
    #[serde(default)]
    pub computer_use_allowed_apps: Vec<ComputerAppGrant>,
}

#[derive(Serialize)]
struct PersistedStateSnapshot<'a> {
    version: u32,
    projects: &'a [Project],
    sessions: Vec<&'a AgentSession>,
    selected_project: Option<Uuid>,
    selected_session: Option<Uuid>,
    last_provider: ProviderKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_model: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_reasoning_effort: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_service_tier: Option<&'a str>,
    favorite_models: &'a [FavoriteModel],
    theme: ThemePreference,
    sidebar_visible: bool,
    right_panel_visible: bool,
    sidebar_width: f32,
    right_panel_width: f32,
    computer_use_enabled: bool,
    computer_use_allowed_apps: &'a [ComputerAppGrant],
}

fn default_sidebar_width() -> f32 {
    DEFAULT_SIDEBAR_WIDTH
}

fn default_right_panel_width() -> f32 {
    DEFAULT_RIGHT_PANEL_WIDTH
}

impl PersistedState {
    pub fn empty() -> Self {
        Self {
            version: STATE_VERSION,
            projects: Vec::new(),
            sessions: Vec::new(),
            selected_project: None,
            selected_session: None,
            last_provider: ProviderKind::Codex,
            last_model: None,
            last_reasoning_effort: None,
            last_service_tier: None,
            favorite_models: Vec::new(),
            theme: ThemePreference::System,
            sidebar_visible: true,
            right_panel_visible: true,
            sidebar_width: DEFAULT_SIDEBAR_WIDTH,
            right_panel_width: DEFAULT_RIGHT_PANEL_WIDTH,
            computer_use_enabled: true,
            computer_use_allowed_apps: Vec::new(),
        }
    }

    pub fn fresh(cwd: PathBuf) -> Self {
        let project = Project::from_path(cwd);
        let session = AgentSession::new(project.id, ProviderKind::Codex);
        Self {
            version: STATE_VERSION,
            selected_project: Some(project.id),
            selected_session: Some(session.id),
            projects: vec![project],
            sessions: vec![session],
            last_provider: ProviderKind::Codex,
            last_model: None,
            last_reasoning_effort: None,
            last_service_tier: None,
            favorite_models: Vec::new(),
            theme: ThemePreference::System,
            sidebar_visible: true,
            right_panel_visible: true,
            sidebar_width: DEFAULT_SIDEBAR_WIDTH,
            right_panel_width: DEFAULT_RIGHT_PANEL_WIDTH,
            computer_use_enabled: true,
            computer_use_allowed_apps: Vec::new(),
        }
    }

    pub fn new_session(&self, project_id: Uuid, provider: ProviderKind) -> AgentSession {
        let mut session = AgentSession::new(project_id, provider);
        if provider == self.last_provider {
            session.model.clone_from(&self.last_model);
            session
                .reasoning_effort
                .clone_from(&self.last_reasoning_effort);
            session.service_tier.clone_from(&self.last_service_tier);
        }
        session
    }

    fn ensure_runtime_session(&mut self) {
        if self.selected_session.is_some_and(|selected_session| {
            self.sessions
                .iter()
                .any(|session| session.id == selected_session)
        }) {
            return;
        }
        self.selected_session = None;
        let Some(project_id) = self.selected_project.filter(|selected_project| {
            self.projects
                .iter()
                .any(|project| project.id == *selected_project)
        }) else {
            return;
        };
        let session = self.new_session(project_id, self.last_provider);
        self.selected_session = Some(session.id);
        self.sessions.push(session);
    }

    fn storage_snapshot(&self) -> PersistedStateSnapshot<'_> {
        let sessions = self
            .sessions
            .iter()
            .filter(|session| session.has_started())
            .collect::<Vec<_>>();
        let selected_session = self
            .selected_session
            .filter(|selected_session| {
                sessions
                    .iter()
                    .any(|session| session.id == *selected_session)
            })
            .or_else(|| {
                self.selected_project.and_then(|selected_project| {
                    sessions
                        .iter()
                        .filter(|session| session.project_id == selected_project)
                        .max_by_key(|session| session.updated_at)
                        .map(|session| session.id)
                })
            });
        PersistedStateSnapshot {
            version: self.version,
            projects: &self.projects,
            sessions,
            selected_project: self.selected_project,
            selected_session,
            last_provider: self.last_provider,
            last_model: self.last_model.as_deref(),
            last_reasoning_effort: self.last_reasoning_effort.as_deref(),
            last_service_tier: self.last_service_tier.as_deref(),
            favorite_models: &self.favorite_models,
            theme: self.theme,
            sidebar_visible: self.sidebar_visible,
            right_panel_visible: self.right_panel_visible,
            sidebar_width: self.sidebar_width,
            right_panel_width: self.right_panel_width,
            computer_use_enabled: self.computer_use_enabled,
            computer_use_allowed_apps: &self.computer_use_allowed_apps,
        }
    }
}

pub struct StateStore {
    path: PathBuf,
}

impl StateStore {
    pub fn default_path() -> PathBuf {
        dirs::data_local_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join(DATA_DIRECTORY_NAME)
            .join("state.json")
    }

    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn load_or_fresh(&self, cwd: PathBuf) -> PersistedState {
        let mut state = self.load().unwrap_or_else(|_| {
            if cwd.parent().is_none() {
                PersistedState::empty()
            } else {
                PersistedState::fresh(cwd)
            }
        });
        state.ensure_runtime_session();
        state
    }

    pub fn load(&self) -> io::Result<PersistedState> {
        let bytes = fs::read(&self.path)?;
        let mut state = serde_json::from_slice::<PersistedState>(&bytes)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        if !(OLDEST_SUPPORTED_STATE_VERSION..=STATE_VERSION).contains(&state.version) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unsupported Waku state version",
            ));
        }
        if state.version < 3 {
            for session in &mut state.sessions {
                session.migrate_pre_access_modes();
            }
        }
        state.version = STATE_VERSION;
        for session in &mut state.sessions {
            session.migrate_legacy_state();
        }
        normalize_computer_app_grants(&mut state.computer_use_allowed_apps);
        if let Some(session) = state
            .selected_session
            .and_then(|selected_session| {
                state
                    .sessions
                    .iter()
                    .find(|session| session.id == selected_session)
            })
            .cloned()
        {
            if state.last_model.is_none() {
                state.last_model = session.model;
            }
            if state.last_reasoning_effort.is_none() {
                state.last_reasoning_effort = session.reasoning_effort;
            }
            if state.last_service_tier.is_none() {
                state.last_service_tier = session.service_tier;
            }
        }
        Ok(state)
    }

    pub fn save(&self, state: &PersistedState) -> io::Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let data = serde_json::to_vec_pretty(&state.storage_snapshot())
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        let temporary_path = temporary_path(&self.path);
        fs::write(&temporary_path, data)?;
        fs::rename(temporary_path, &self.path)
    }
}

fn normalize_computer_app_grants(grants: &mut Vec<ComputerAppGrant>) {
    let mut seen_bundle_ids = HashSet::new();
    grants.retain(|grant| {
        !grant.bundle_id.trim().is_empty() && seen_bundle_ids.insert(grant.bundle_id.clone())
    });
}

fn temporary_path(path: &Path) -> PathBuf {
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(".tmp");
    PathBuf::from(temporary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        ActivityItem, ActivityKind, FavoriteModel, ReasoningBlock, TranscriptBlock,
        TranscriptBlockContent,
    };

    #[test]
    fn default_path_uses_build_specific_data_directory() {
        let path = StateStore::default_path();
        let data_directory = path.parent().and_then(Path::file_name);

        #[cfg(debug_assertions)]
        assert_eq!(data_directory, Some(std::ffi::OsStr::new("Waku Debug")));
        #[cfg(not(debug_assertions))]
        assert_eq!(data_directory, Some(std::ffi::OsStr::new("Waku")));
    }

    #[test]
    fn state_round_trips() {
        let directory = std::env::temp_dir().join(format!("waku-state-{}", Uuid::new_v4()));
        let store = StateStore::new(directory.join("state.json"));
        let mut state = PersistedState::fresh(PathBuf::from("/tmp/project"));
        state.sessions[0].model = Some("gpt-5.6-luna".into());
        state.last_model = Some("gpt-5.6-luna".into());
        state.sessions[0].reasoning_effort = Some("xhigh".into());
        state.last_reasoning_effort = Some("xhigh".into());
        state.sessions[0].service_tier = Some("fast".into());
        state.last_service_tier = Some("fast".into());
        state.sessions[0].runtime_mode = crate::model::RuntimeMode::Auto;
        state.favorite_models.push(FavoriteModel {
            provider: ProviderKind::Codex,
            model: "gpt-5.6-luna".into(),
        });
        state.theme = ThemePreference::Light;
        state.sidebar_visible = false;
        state.right_panel_visible = false;
        state.sidebar_width = 318.0;
        state.right_panel_width = 612.0;
        state.computer_use_enabled = false;
        state.computer_use_allowed_apps.push(ComputerAppGrant {
            bundle_id: "com.apple.Safari".into(),
            app_name: "Safari".into(),
        });
        state.sessions[0].begin_turn("Persist this session");
        state.sessions[0].finish_active_turn(crate::model::TurnStatus::Completed);
        state.sessions[0].transcript_blocks.extend([
            TranscriptBlock {
                after_message: 1,
                turn_id: None,
                content: TranscriptBlockContent::Reasoning(ReasoningBlock {
                    content: "Checking the source".into(),
                    started_at_ms: 1_000,
                    finished_at_ms: 2_500,
                }),
            },
            TranscriptBlock {
                after_message: 1,
                turn_id: None,
                content: TranscriptBlockContent::Activities(vec![ActivityItem::new(
                    Some("tool-1".into()),
                    ActivityKind::Search,
                    "Read src/main.rs",
                    Some("{\"path\":\"src/main.rs\"}".into()),
                    true,
                )]),
            },
        ]);
        store.save(&state).unwrap();
        let restored = store.load().unwrap();
        assert_eq!(restored.projects[0].name, "project");
        assert_eq!(restored.sessions.len(), 1);
        assert_eq!(restored.sessions[0].model.as_deref(), Some("gpt-5.6-luna"));
        assert_eq!(restored.last_model.as_deref(), Some("gpt-5.6-luna"));
        assert_eq!(restored.last_reasoning_effort.as_deref(), Some("xhigh"));
        assert_eq!(restored.last_service_tier.as_deref(), Some("fast"));
        assert_eq!(
            restored.sessions[0].reasoning_effort.as_deref(),
            Some("xhigh")
        );
        assert_eq!(restored.sessions[0].service_tier.as_deref(), Some("fast"));
        assert_eq!(
            restored.sessions[0].runtime_mode,
            crate::model::RuntimeMode::Auto
        );
        assert_eq!(restored.favorite_models, state.favorite_models);
        assert_eq!(restored.theme, ThemePreference::Light);
        assert!(!restored.sidebar_visible);
        assert!(!restored.right_panel_visible);
        assert_eq!(restored.sidebar_width, 318.0);
        assert_eq!(restored.right_panel_width, 612.0);
        assert!(!restored.computer_use_enabled);
        assert_eq!(
            restored.computer_use_allowed_apps,
            state.computer_use_allowed_apps
        );
        assert_eq!(restored.sessions[0].transcript_blocks.len(), 2);
        assert!(matches!(
            &restored.sessions[0].transcript_blocks[0].content,
            TranscriptBlockContent::Reasoning(reasoning)
                if reasoning.content == "Checking the source"
        ));
        fs::remove_dir_all(directory).ok();
    }

    #[test]
    fn legacy_signed_computer_grants_migrate_to_bundle_ids() {
        let legacy = serde_json::json!({
            "bundleId": "net.imput.helium",
            "teamId": "S4Q33XPHB4",
            "appName": "Helium"
        });
        let grant: ComputerAppGrant = serde_json::from_value(legacy).unwrap();
        assert_eq!(grant.key(), "net.imput.helium");

        let mut grants = vec![
            grant,
            ComputerAppGrant {
                bundle_id: "net.imput.helium".into(),
                app_name: "Helium Preview".into(),
            },
            ComputerAppGrant {
                bundle_id: String::new(),
                app_name: "Missing identity".into(),
            },
        ];
        normalize_computer_app_grants(&mut grants);
        assert_eq!(grants.len(), 1);
        assert_eq!(grants[0].app_name, "Helium");

        let saved = serde_json::to_value(&grants[0]).unwrap();
        assert_eq!(
            saved.get("bundleId").and_then(|value| value.as_str()),
            Some("net.imput.helium")
        );
        assert!(saved.get("teamId").is_none());
    }

    #[test]
    fn blank_sessions_stay_runtime_only() {
        let directory = std::env::temp_dir().join(format!("waku-draft-{}", Uuid::new_v4()));
        let store = StateStore::new(directory.join("state.json"));
        let state = PersistedState::fresh(PathBuf::from("/tmp/project"));

        store.save(&state).unwrap();
        let restored = store.load().unwrap();

        assert!(restored.sessions.is_empty());
        assert!(restored.selected_session.is_none());
        assert_eq!(restored.selected_project, state.selected_project);
        fs::remove_dir_all(directory).ok();
    }

    #[test]
    fn selected_draft_falls_back_to_latest_started_session_on_disk() {
        let directory = std::env::temp_dir().join(format!("waku-draft-{}", Uuid::new_v4()));
        let store = StateStore::new(directory.join("state.json"));
        let mut state = PersistedState::fresh(PathBuf::from("/tmp/project"));
        let started_id = state.sessions[0].id;
        state.sessions[0].begin_turn("Persist this session");
        state.sessions[0].finish_active_turn(crate::model::TurnStatus::Completed);
        let draft = state.new_session(state.projects[0].id, ProviderKind::Codex);
        state.selected_session = Some(draft.id);
        state.sessions.push(draft);

        store.save(&state).unwrap();
        let restored = store.load().unwrap();

        assert_eq!(restored.sessions.len(), 1);
        assert_eq!(restored.sessions[0].id, started_id);
        assert_eq!(restored.selected_session, Some(started_id));
        fs::remove_dir_all(directory).ok();
    }

    #[test]
    fn sessions_without_transcript_blocks_remain_compatible() {
        let session = AgentSession::new(Uuid::new_v4(), ProviderKind::Grok);
        let mut value = serde_json::to_value(session).unwrap();
        value.as_object_mut().unwrap().remove("transcript_blocks");

        let restored = serde_json::from_value::<AgentSession>(value).unwrap();
        assert!(restored.transcript_blocks.is_empty());
    }

    #[test]
    fn version_one_sessions_migrate_provider_ids_and_turns() {
        let directory = std::env::temp_dir().join(format!("waku-v1-state-{}", Uuid::new_v4()));
        let path = directory.join("state.json");
        let store = StateStore::new(path.clone());
        let mut state = PersistedState::fresh(PathBuf::from("/tmp/project"));
        state.version = 1;
        state.sessions[0].messages.push(crate::model::Message::new(
            crate::model::MessageRole::User,
            "hello",
        ));
        state.sessions[0].messages.push(crate::model::Message::new(
            crate::model::MessageRole::Assistant,
            "hi",
        ));
        let mut value = serde_json::to_value(&state).unwrap();
        value.as_object_mut().unwrap().remove("favorite_models");
        value.as_object_mut().unwrap().remove("theme");
        value.as_object_mut().unwrap().remove("last_model");
        value
            .as_object_mut()
            .unwrap()
            .remove("last_reasoning_effort");
        value.as_object_mut().unwrap().remove("last_service_tier");
        value.as_object_mut().unwrap().remove("sidebar_visible");
        value.as_object_mut().unwrap().remove("right_panel_visible");
        value.as_object_mut().unwrap().remove("sidebar_width");
        value.as_object_mut().unwrap().remove("right_panel_width");
        value["sessions"][0]
            .as_object_mut()
            .unwrap()
            .remove("model");
        value["sessions"][0]["provider_session_id"] =
            serde_json::Value::String("thread-123".into());
        fs::create_dir_all(&directory).unwrap();
        fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();

        let restored = store.load().unwrap();
        assert_eq!(restored.version, STATE_VERSION);
        assert!(restored.favorite_models.is_empty());
        assert_eq!(restored.theme, ThemePreference::System);
        assert!(restored.last_model.is_none());
        assert!(restored.last_reasoning_effort.is_none());
        assert!(restored.last_service_tier.is_none());
        assert!(restored.sidebar_visible);
        assert!(restored.right_panel_visible);
        assert_eq!(restored.sidebar_width, DEFAULT_SIDEBAR_WIDTH);
        assert_eq!(restored.right_panel_width, DEFAULT_RIGHT_PANEL_WIDTH);
        assert!(restored.sessions[0].model.is_none());
        assert_eq!(
            restored.sessions[0]
                .provider_cursor
                .as_ref()
                .map(crate::model::ProviderResumeCursor::native_id),
            Some("thread-123")
        );
        assert_eq!(restored.sessions[0].turns.len(), 1);
        assert!(
            restored.sessions[0]
                .messages
                .iter()
                .all(|message| message.turn_id.is_some())
        );

        fs::remove_dir_all(directory).ok();
    }

    #[test]
    fn selected_model_and_traits_are_used_for_new_sessions() {
        let mut state = PersistedState::fresh(PathBuf::from("/tmp/project"));
        state.last_provider = ProviderKind::Grok;
        state.last_model = Some("grok-code-fast-1".into());
        state.last_reasoning_effort = Some("high".into());
        state.last_service_tier = Some("fast".into());

        let remembered = state.new_session(state.projects[0].id, ProviderKind::Grok);
        let other_provider = state.new_session(state.projects[0].id, ProviderKind::Codex);

        assert_eq!(remembered.model.as_deref(), Some("grok-code-fast-1"));
        assert_eq!(remembered.reasoning_effort.as_deref(), Some("high"));
        assert_eq!(remembered.service_tier.as_deref(), Some("fast"));
        assert!(other_provider.model.is_none());
        assert!(other_provider.reasoning_effort.is_none());
        assert!(other_provider.service_tier.is_none());
    }

    #[test]
    fn missing_remembered_selection_is_backfilled_from_selected_session() {
        let directory = std::env::temp_dir().join(format!("waku-model-{}", Uuid::new_v4()));
        let path = directory.join("state.json");
        let store = StateStore::new(path.clone());
        let mut state = PersistedState::fresh(PathBuf::from("/tmp/project"));
        state.sessions[0].model = Some("gpt-5.6-luna".into());
        state.sessions[0].reasoning_effort = Some("xhigh".into());
        state.sessions[0].service_tier = Some("fast".into());
        let mut value = serde_json::to_value(state).unwrap();
        value.as_object_mut().unwrap().remove("last_model");
        value
            .as_object_mut()
            .unwrap()
            .remove("last_reasoning_effort");
        value.as_object_mut().unwrap().remove("last_service_tier");
        fs::create_dir_all(&directory).unwrap();
        fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();

        let restored = store.load().unwrap();

        assert_eq!(restored.last_model.as_deref(), Some("gpt-5.6-luna"));
        assert_eq!(restored.last_reasoning_effort.as_deref(), Some("xhigh"));
        assert_eq!(restored.last_service_tier.as_deref(), Some("fast"));
        fs::remove_dir_all(directory).ok();
    }

    #[test]
    fn version_two_combined_modes_migrate_to_access_and_interaction_settings() {
        let directory = std::env::temp_dir().join(format!("waku-v2-{}", Uuid::new_v4()));
        let path = directory.join("state.json");
        let store = StateStore::new(path.clone());
        let mut state = PersistedState::fresh(PathBuf::from("/tmp/project"));
        state.version = 2;
        state.sessions[0].runtime_mode = crate::model::RuntimeMode::Plan;
        let mut auto_session =
            AgentSession::new(state.projects[0].id, crate::model::ProviderKind::Codex);
        auto_session.runtime_mode = crate::model::RuntimeMode::Auto;
        state.sessions.push(auto_session);
        fs::create_dir_all(&directory).unwrap();
        fs::write(&path, serde_json::to_vec(&state).unwrap()).unwrap();

        let restored = store.load().unwrap();
        assert_eq!(restored.version, STATE_VERSION);
        assert_eq!(
            restored.sessions[0].runtime_mode,
            crate::model::RuntimeMode::Ask
        );
        assert_eq!(
            restored.sessions[0].interaction_mode,
            crate::model::InteractionMode::Plan
        );
        assert_eq!(
            restored.sessions[1].runtime_mode,
            crate::model::RuntimeMode::AutoAcceptEdits
        );
        assert_eq!(
            restored.sessions[1].interaction_mode,
            crate::model::InteractionMode::Build
        );

        fs::remove_dir_all(directory).ok();
    }

    #[test]
    fn app_bundle_root_directory_starts_with_onboarding() {
        let store = StateStore::new(
            std::env::temp_dir()
                .join(format!("waku-empty-{}", Uuid::new_v4()))
                .join("state.json"),
        );
        let state = store.load_or_fresh(PathBuf::from("/"));
        assert!(state.projects.is_empty());
        assert!(state.selected_session.is_none());
    }
}

//! Desktop-owned preferences and RPC proxies for daemon-owned task state.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{Command, DaemonExposureSettings, DaemonSettings, DaemonSupervisor, ResponsePayload};
use waku_protocol::automation::{Automation, AutomationChange};
use waku_protocol::computer_use::ComputerAppGrant;
use waku_protocol::i18n::AppLanguage;
use waku_protocol::identity::DATA_DIRECTORY_NAME;
use waku_protocol::model::{AgentSession, FavoriteModel, Project, ProviderKind};
use waku_protocol::theme::ThemePreference;

pub use waku_protocol::persistence::{
    ComposerDraft, ComposerDraftAttachment, ComposerDraftChange, ComposerDraftKey,
    ComposerDraftTarget, ComposerDrafts, SessionMessageMatch,
};

const STATE_VERSION: u32 = 5;
const APP_STATE_VERSION: u32 = 1;

pub const DEFAULT_SIDEBAR_WIDTH: f32 = 252.0;
pub const DEFAULT_RIGHT_PANEL_WIDTH: f32 = 460.0;

fn default_sidebar_visibility() -> bool {
    true
}

fn default_right_panel_visibility() -> bool {
    false
}

fn default_computer_use_enabled() -> bool {
    false
}

fn default_analytics_enabled() -> bool {
    true
}

fn default_provider() -> ProviderKind {
    ProviderKind::Codex
}

fn default_sidebar_width() -> f32 {
    DEFAULT_SIDEBAR_WIDTH
}

fn default_right_panel_width() -> f32 {
    DEFAULT_RIGHT_PANEL_WIDTH
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RememberedModelTraits {
    provider: ProviderKind,
    model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    service_tier: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    context_window: Option<String>,
}

/// Remote composer-draft proxy. Draft bytes and attachments remain owned by
/// the daemon even though the desktop keeps an in-memory editing snapshot.
#[derive(Clone)]
pub struct ComposerDraftStore {
    daemon: DaemonSupervisor,
    save_state: Arc<Mutex<ComposerDraftSaveState>>,
}

#[derive(Default)]
struct ComposerDraftSaveState {
    snapshot: ComposerDrafts,
    latest_generation: u64,
}

impl ComposerDraftStore {
    pub fn remote(daemon: DaemonSupervisor) -> Self {
        Self {
            daemon,
            save_state: Arc::new(Mutex::new(ComposerDraftSaveState::default())),
        }
    }

    pub fn load(&self) -> io::Result<ComposerDrafts> {
        match self
            .daemon
            .client()
            .request(Uuid::nil(), Uuid::nil(), Command::LoadComposerDrafts)
            .map_err(to_io_error)?
        {
            ResponsePayload::ComposerDrafts { drafts } => {
                self.save_state.lock().snapshot = drafts.clone();
                Ok(drafts)
            }
            _ => Err(io::Error::other(
                "Waku daemon returned an invalid composer-drafts response",
            )),
        }
    }

    pub fn save(&self, drafts: ComposerDrafts, generation: u64) -> io::Result<()> {
        // Serialize this client's detached background saves and compare only
        // against its last local snapshot. A second client may add or remove
        // other keys without those changes being interpreted as ours.
        let mut state = self.save_state.lock();
        if generation < state.latest_generation {
            return Ok(());
        }
        let changes = composer_draft_changes(&state.snapshot, &drafts);
        if changes.is_empty() {
            state.latest_generation = generation;
            return Ok(());
        }
        match self
            .daemon
            .client()
            .request(
                Uuid::nil(),
                Uuid::nil(),
                Command::ApplyComposerDraftChanges { changes },
            )
            .map_err(to_io_error)?
        {
            ResponsePayload::Ack => {
                state.snapshot = drafts;
                state.latest_generation = generation;
                Ok(())
            }
            _ => Err(io::Error::other(
                "Waku daemon returned an invalid composer-drafts save response",
            )),
        }
    }
}

fn composer_draft_changes(
    previous: &ComposerDrafts,
    next: &ComposerDrafts,
) -> Vec<ComposerDraftChange> {
    let mut changes = Vec::new();
    collect_composer_draft_changes(
        &previous.new_sessions,
        &next.new_sessions,
        |project_id| ComposerDraftTarget::NewSession { project_id },
        &mut changes,
    );
    collect_composer_draft_changes(
        &previous.sessions,
        &next.sessions,
        |session_id| ComposerDraftTarget::Session { session_id },
        &mut changes,
    );
    changes
}

fn collect_composer_draft_changes(
    previous: &HashMap<Uuid, ComposerDraft>,
    next: &HashMap<Uuid, ComposerDraft>,
    target: impl Fn(Uuid) -> ComposerDraftTarget,
    changes: &mut Vec<ComposerDraftChange>,
) {
    for (id, draft) in next {
        if previous.get(id) != Some(draft) {
            changes.push(ComposerDraftChange {
                target: target(*id),
                draft: Some(draft.clone()),
            });
        }
    }
    for id in previous.keys() {
        if !next.contains_key(id) {
            changes.push(ComposerDraftChange {
                target: target(*id),
                draft: None,
            });
        }
    }
}

/// Last observed main-window frame in logical pixels. GPUI window bounds are
/// relative to the display the window sits on, so the frame only means
/// something together with `display` — the stable display UUID (Zed persists
/// the same pair; display *ids* renumber across reboots and replugs). While
/// the window is maximized or fullscreen these bounds keep the floating frame
/// the window returns to, not the screen-filling one.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
pub struct PersistedWindowState {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    #[serde(default)]
    pub maximized: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<Uuid>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct AppSettings {
    pub analytics_enabled: bool,
    pub favorite_models: Vec<FavoriteModel>,
    pub theme: ThemePreference,
    pub language: AppLanguage,
    pub daemon_exposure: DaemonExposureSettings,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            analytics_enabled: default_analytics_enabled(),
            favorite_models: Vec::new(),
            theme: ThemePreference::System,
            language: AppLanguage::default(),
            daemon_exposure: DaemonExposureSettings::default(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct AppState {
    app_state_version: u32,
    #[serde(default = "Uuid::new_v4")]
    analytics_id: Uuid,
    #[serde(default)]
    selected_project: Option<Uuid>,
    #[serde(default)]
    selected_session: Option<Uuid>,
    #[serde(default = "default_provider")]
    last_provider: ProviderKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_reasoning_effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_service_tier: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_context_window: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    remembered_model_traits: Vec<RememberedModelTraits>,
    #[serde(default = "default_sidebar_visibility")]
    sidebar_visible: bool,
    #[serde(default = "default_right_panel_visibility")]
    right_panel_visible: bool,
    #[serde(default = "default_sidebar_width")]
    sidebar_width: f32,
    #[serde(default = "default_right_panel_width")]
    right_panel_width: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    window_state: Option<PersistedWindowState>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PersistedState {
    pub version: u32,
    #[serde(default = "Uuid::new_v4")]
    pub analytics_id: Uuid,
    #[serde(default = "default_analytics_enabled")]
    pub analytics_enabled: bool,
    pub projects: Vec<Project>,
    pub sessions: Vec<AgentSession>,
    /// Saved scheduling automations mirrored from the daemon. Local authoring
    /// queues per-automation changes so saving never replaces unrelated state.
    #[serde(default)]
    pub automations: Vec<Automation>,
    pub selected_project: Option<Uuid>,
    pub selected_session: Option<Uuid>,
    pub last_provider: ProviderKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_reasoning_effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_service_tier: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_context_window: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remembered_model_traits: Vec<RememberedModelTraits>,
    #[serde(default)]
    pub favorite_models: Vec<FavoriteModel>,
    #[serde(default)]
    pub theme: ThemePreference,
    #[serde(default)]
    pub language: AppLanguage,
    #[serde(default)]
    pub daemon_exposure: DaemonExposureSettings,
    #[serde(default = "default_sidebar_visibility")]
    pub sidebar_visible: bool,
    #[serde(default = "default_right_panel_visibility")]
    pub right_panel_visible: bool,
    #[serde(default = "default_sidebar_width")]
    pub sidebar_width: f32,
    #[serde(default = "default_right_panel_width")]
    pub right_panel_width: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_state: Option<PersistedWindowState>,
    #[serde(default = "default_computer_use_enabled")]
    pub computer_use_enabled: bool,
    #[serde(default)]
    pub computer_use_allowed_apps: Vec<ComputerAppGrant>,
    #[serde(default)]
    pub disabled_providers: Vec<ProviderKind>,
    #[serde(default)]
    pub provider_binary_overrides: HashMap<ProviderKind, String>,
    #[serde(skip)]
    daemon_settings_extra: BTreeMap<String, serde_json::Value>,
    #[serde(skip)]
    dirty_sessions: HashSet<Uuid>,
    #[serde(skip)]
    pending_automation_changes: Vec<AutomationChange>,
}

impl PersistedState {
    pub fn session_mut(&mut self, id: Uuid) -> Option<&mut AgentSession> {
        let session = self.sessions.iter_mut().find(|session| session.id == id)?;
        self.dirty_sessions.insert(id);
        Some(session)
    }

    pub fn mark_session_dirty(&mut self, id: Uuid) {
        self.dirty_sessions.insert(id);
    }

    pub fn push_session(&mut self, session: AgentSession) {
        self.dirty_sessions.insert(session.id);
        self.sessions.push(session);
    }

    pub fn automation(&self, id: Uuid) -> Option<&Automation> {
        self.automations
            .iter()
            .find(|automation| automation.id == id)
    }

    pub fn automation_mut(&mut self, id: Uuid) -> Option<&mut Automation> {
        self.automations
            .iter_mut()
            .find(|automation| automation.id == id)
    }

    pub fn push_automation(&mut self, automation: Automation) {
        self.automations.push(automation);
    }

    /// Removes an automation, returning whether one was found.
    pub fn remove_automation(&mut self, id: Uuid) -> bool {
        let before = self.automations.len();
        self.automations.retain(|automation| automation.id != id);
        self.automations.len() != before
    }

    pub fn queue_automation_upsert(&mut self, automation: Automation) {
        self.pending_automation_changes
            .retain(|change| change.automation_id() != automation.id);
        self.pending_automation_changes
            .push(AutomationChange::Upsert { automation });
    }

    pub fn queue_automation_removal(&mut self, automation_id: Uuid, cascade_sessions: bool) {
        self.pending_automation_changes
            .retain(|change| change.automation_id() != automation_id);
        self.pending_automation_changes
            .push(AutomationChange::Remove {
                automation_id,
                cascade_sessions,
            });
    }

    fn take_automation_changes(&mut self) -> Vec<AutomationChange> {
        std::mem::take(&mut self.pending_automation_changes)
    }

    fn restore_automation_changes(&mut self, mut changes: Vec<AutomationChange>) {
        changes.append(&mut self.pending_automation_changes);
        self.pending_automation_changes = changes;
    }

    pub fn empty() -> Self {
        Self {
            version: STATE_VERSION,
            analytics_id: Uuid::new_v4(),
            analytics_enabled: true,
            projects: Vec::new(),
            sessions: Vec::new(),
            automations: Vec::new(),
            selected_project: None,
            selected_session: None,
            last_provider: ProviderKind::Codex,
            last_model: None,
            last_reasoning_effort: None,
            last_service_tier: None,
            last_context_window: None,
            remembered_model_traits: Vec::new(),
            favorite_models: Vec::new(),
            theme: ThemePreference::System,
            language: AppLanguage::default(),
            daemon_exposure: DaemonExposureSettings::default(),
            sidebar_visible: true,
            right_panel_visible: false,
            sidebar_width: DEFAULT_SIDEBAR_WIDTH,
            right_panel_width: DEFAULT_RIGHT_PANEL_WIDTH,
            window_state: None,
            computer_use_enabled: false,
            computer_use_allowed_apps: Vec::new(),
            disabled_providers: Vec::new(),
            provider_binary_overrides: HashMap::new(),
            daemon_settings_extra: BTreeMap::new(),
            dirty_sessions: HashSet::new(),
            pending_automation_changes: Vec::new(),
        }
    }

    pub fn fresh(cwd: PathBuf) -> Self {
        let project = Project::from_path(cwd);
        let session = AgentSession::new(project.id, ProviderKind::Codex);
        Self {
            selected_project: Some(project.id),
            selected_session: Some(session.id),
            projects: vec![project],
            sessions: vec![session],
            ..Self::empty()
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
            session.context_window.clone_from(&self.last_context_window);
        }
        session
    }

    pub fn remember_model_traits(
        &mut self,
        provider: ProviderKind,
        model: &str,
        reasoning_effort: Option<String>,
        service_tier: Option<String>,
        context_window: Option<String>,
    ) {
        let existing = self
            .remembered_model_traits
            .iter()
            .position(|traits| traits.provider == provider && traits.model == model);
        if reasoning_effort.is_none() && service_tier.is_none() && context_window.is_none() {
            if let Some(index) = existing {
                self.remembered_model_traits.remove(index);
            }
            return;
        }
        if let Some(index) = existing {
            let traits = &mut self.remembered_model_traits[index];
            traits.reasoning_effort = reasoning_effort;
            traits.service_tier = service_tier;
            traits.context_window = context_window;
        } else {
            self.remembered_model_traits.push(RememberedModelTraits {
                provider,
                model: model.to_owned(),
                reasoning_effort,
                service_tier,
                context_window,
            });
        }
    }

    pub fn model_traits_for(
        &self,
        provider: ProviderKind,
        model: &str,
    ) -> (Option<String>, Option<String>, Option<String>) {
        self.remembered_model_traits
            .iter()
            .find(|traits| traits.provider == provider && traits.model == model)
            .map(|traits| {
                (
                    traits.reasoning_effort.clone(),
                    traits.service_tier.clone(),
                    traits.context_window.clone(),
                )
            })
            .unwrap_or_default()
    }

    pub fn daemon_settings(&self) -> DaemonSettings {
        DaemonSettings {
            computer_use_enabled: self.computer_use_enabled,
            computer_use_allowed_apps: self.computer_use_allowed_apps.clone(),
            disabled_providers: self.disabled_providers.clone(),
            provider_binary_overrides: self.provider_binary_overrides.clone(),
            extra: self.daemon_settings_extra.clone(),
        }
    }

    pub fn apply_daemon_settings(&mut self, settings: DaemonSettings) {
        self.computer_use_enabled = settings.computer_use_enabled;
        self.computer_use_allowed_apps = settings.computer_use_allowed_apps;
        self.disabled_providers = settings.disabled_providers;
        self.provider_binary_overrides = settings.provider_binary_overrides;
        self.daemon_settings_extra = settings.extra;
    }

    fn app_settings(&self) -> AppSettings {
        AppSettings {
            analytics_enabled: self.analytics_enabled,
            favorite_models: self.favorite_models.clone(),
            theme: self.theme,
            language: self.language,
            daemon_exposure: self.daemon_exposure.clone(),
        }
    }

    fn app_state(&self) -> AppState {
        AppState {
            app_state_version: APP_STATE_VERSION,
            analytics_id: self.analytics_id,
            selected_project: self.selected_project,
            selected_session: self.persistable_selected_session(),
            last_provider: self.last_provider,
            last_model: self.last_model.clone(),
            last_reasoning_effort: self.last_reasoning_effort.clone(),
            last_service_tier: self.last_service_tier.clone(),
            last_context_window: self.last_context_window.clone(),
            remembered_model_traits: self.remembered_model_traits.clone(),
            sidebar_visible: self.sidebar_visible,
            right_panel_visible: self.right_panel_visible,
            sidebar_width: self.sidebar_width,
            right_panel_width: self.right_panel_width,
            window_state: self.window_state,
        }
    }

    fn apply_app_settings(&mut self, settings: AppSettings) {
        self.analytics_enabled = settings.analytics_enabled;
        self.favorite_models = settings.favorite_models;
        self.theme = settings.theme;
        self.language = settings.language;
        self.daemon_exposure = settings.daemon_exposure;
    }

    fn apply_app_state(&mut self, app_state: AppState) {
        self.analytics_id = app_state.analytics_id;
        self.selected_project = app_state.selected_project;
        self.selected_session = app_state.selected_session;
        self.last_provider = app_state.last_provider;
        self.last_model = app_state.last_model;
        self.last_reasoning_effort = app_state.last_reasoning_effort;
        self.last_service_tier = app_state.last_service_tier;
        self.last_context_window = app_state.last_context_window;
        self.remembered_model_traits = app_state.remembered_model_traits;
        self.sidebar_visible = app_state.sidebar_visible;
        self.right_panel_visible = app_state.right_panel_visible;
        self.sidebar_width = app_state.sidebar_width;
        self.right_panel_width = app_state.right_panel_width;
        self.window_state = app_state.window_state;
    }

    fn persistable_selected_session(&self) -> Option<Uuid> {
        self.selected_session.filter(|selected| {
            self.sessions
                .iter()
                .any(|session| session.id == *selected && session.has_started())
        })
    }

    fn ensure_runtime_session(&mut self) {
        if self
            .selected_session
            .is_some_and(|selected| self.sessions.iter().any(|session| session.id == selected))
        {
            return;
        }
        self.selected_session = None;
        let Some(project_id) = self
            .selected_project
            .filter(|selected| self.projects.iter().any(|project| project.id == *selected))
        else {
            return;
        };
        let session = self.new_session(project_id, self.last_provider);
        self.selected_session = Some(session.id);
        self.sessions.push(session);
    }

    fn migrate_loaded(&mut self) {
        for session in &mut self.sessions {
            let checkpoint_totals_current = session.turns.iter().all(|turn| {
                turn.checkpoint
                    .as_ref()
                    .is_none_or(waku_protocol::model::Checkpoint::totals_are_current)
            });
            let before = (
                session.turns.len(),
                session.last_reply_at,
                session.provider_cursor.is_some(),
            );
            session.migrate_legacy_state();
            session.backfill_last_reply_at();
            if !checkpoint_totals_current
                || before
                    != (
                        session.turns.len(),
                        session.last_reply_at,
                        session.provider_cursor.is_some(),
                    )
            {
                self.dirty_sessions.insert(session.id);
            }
        }
        self.version = STATE_VERSION;
        normalize_computer_app_grants(&mut self.computer_use_allowed_apps);
        self.backfill_remembered_selection();
    }

    fn backfill_remembered_selection(&mut self) {
        let Some(session) = self
            .selected_session
            .and_then(|selected| self.sessions.iter().find(|session| session.id == selected))
            .cloned()
        else {
            return;
        };
        if self.last_model.is_none() {
            self.last_model = session.model;
        }
        if self.last_reasoning_effort.is_none() {
            self.last_reasoning_effort = session.reasoning_effort;
        }
        if self.last_service_tier.is_none() {
            self.last_service_tier = session.service_tier;
        }
        if self.last_context_window.is_none() {
            self.last_context_window = session.context_window;
        }
    }
}

fn configuration_directory() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join(".waku")
}

fn default_app_settings_path() -> PathBuf {
    if cfg!(debug_assertions) {
        StateStore::default_path().with_file_name("app.json")
    } else {
        configuration_directory().join("app.json")
    }
}

fn default_app_state_path() -> PathBuf {
    StateStore::default_path().with_file_name("state.json")
}

fn read_app_state_file(path: &Path) -> Option<AppState> {
    let bytes = fs::read(path).ok()?;
    let app_state = serde_json::from_slice::<AppState>(&bytes).ok()?;
    (app_state.app_state_version == APP_STATE_VERSION).then_some(app_state)
}

/// Read the last persisted window frame ahead of the full state load. The
/// main window opens before the daemon connection and `StateStore` exist, and
/// `WindowOptions` needs its frame at `open_window` time.
pub fn load_window_state() -> Option<PersistedWindowState> {
    read_app_state_file(&default_app_state_path())?.window_state
}

fn default_legacy_settings_paths() -> Vec<PathBuf> {
    if cfg!(debug_assertions) {
        vec![StateStore::default_path().with_file_name("settings.json")]
    } else {
        vec![configuration_directory().join("settings.json")]
    }
}

fn read_app_settings_source(
    app_settings_path: &Path,
    legacy_settings_paths: &[PathBuf],
) -> io::Result<Option<(Vec<u8>, bool)>> {
    match fs::read(app_settings_path) {
        Ok(bytes) => return Ok(Some((bytes, true))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    for path in legacy_settings_paths {
        match fs::read(path) {
            Ok(bytes) => return Ok(Some((bytes, false))),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(None)
}

/// Load the app-owned launch settings before the managed daemon starts.
/// Missing settings are initialized immediately so its bearer token remains
/// stable between this process launch and the later UI state load.
pub fn load_or_create_app_settings() -> io::Result<AppSettings> {
    let path = default_app_settings_path();
    let source = read_app_settings_source(&path, &default_legacy_settings_paths())?;
    let loaded_from_primary = source.as_ref().is_some_and(|(_, primary)| *primary);
    let token_was_persisted = source
        .as_ref()
        .and_then(|(bytes, _)| serde_json::from_slice::<serde_json::Value>(bytes).ok())
        .and_then(|value| {
            value
                .get("daemon_exposure")
                .and_then(|daemon| daemon.get("token"))
                .and_then(serde_json::Value::as_str)
                .map(|token| !token.trim().is_empty())
        })
        .unwrap_or(false);
    let mut settings: AppSettings = source
        .map(|(bytes, _)| serde_json::from_slice::<AppSettings>(&bytes).map_err(to_io_error))
        .transpose()?
        .unwrap_or_default();
    let generated_token = settings.daemon_exposure.ensure_token();
    if !loaded_from_primary || !token_was_persisted || generated_token {
        write_json_atomically(&path, &settings)?;
    }
    Ok(settings)
}

/// Desktop state store: app files stay local, task data crosses RPC.
pub struct StateStore {
    path: PathBuf,
    app_state_path: PathBuf,
    app_settings_path: PathBuf,
    legacy_settings_paths: Vec<PathBuf>,
    daemon: DaemonSupervisor,
    remote_default_cwd: Mutex<Option<PathBuf>>,
    /// A task snapshot may only be written after this client has successfully
    /// loaded the daemon's authoritative state. Falling back to an empty UI
    /// after a transient RPC failure must never turn the next ordinary save
    /// into a destructive replacement of the daemon database.
    task_state_loaded: AtomicBool,
}

impl StateStore {
    pub fn default_path() -> PathBuf {
        if cfg!(debug_assertions) {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .and_then(Path::parent)
                .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")))
                .join("temp")
                .join("app.db")
        } else {
            dirs::data_local_dir()
                .unwrap_or_else(std::env::temp_dir)
                .join(DATA_DIRECTORY_NAME)
                .join("app.db")
        }
    }

    pub fn remote(daemon: DaemonSupervisor) -> Self {
        Self {
            app_state_path: default_app_state_path(),
            app_settings_path: default_app_settings_path(),
            legacy_settings_paths: default_legacy_settings_paths(),
            path: Self::default_path(),
            daemon,
            remote_default_cwd: Mutex::new(None),
            task_state_loaded: AtomicBool::new(false),
        }
    }

    pub fn session_message_search(
        &self,
        query: String,
        limit: usize,
    ) -> impl FnOnce() -> io::Result<Vec<SessionMessageMatch>> + Send + 'static {
        let daemon = self.daemon.clone();
        move || match daemon
            .client()
            .request(
                Uuid::nil(),
                Uuid::nil(),
                Command::SearchSessionMessages { query, limit },
            )
            .map_err(to_io_error)?
        {
            ResponsePayload::SessionMessageMatches { matches } => Ok(matches),
            _ => Err(io::Error::other(
                "Waku daemon returned an invalid message-search response",
            )),
        }
    }

    pub fn load_or_fresh(&self, cwd: PathBuf) -> PersistedState {
        let mut state = match self.load() {
            Ok(state) => {
                self.task_state_loaded.store(true, Ordering::Release);
                state
            }
            Err(_) if cwd.parent().is_none() => PersistedState::empty(),
            Err(_) => PersistedState::fresh(cwd),
        };
        if state.projects.is_empty()
            && let Some(cwd) = self.remote_default_cwd.lock().clone()
            && cwd.parent().is_some()
        {
            let project = Project::from_path(cwd);
            let session = state.new_session(project.id, state.last_provider);
            state.selected_project = Some(project.id);
            state.selected_session = Some(session.id);
            state.projects.push(project);
            state.push_session(session);
        }
        state.ensure_runtime_session();
        if let Some(selected) = state.selected_session
            && let Some(session) = state
                .sessions
                .iter_mut()
                .find(|session| session.id == selected)
        {
            let _ = self.hydrate(session);
        }
        state
    }

    pub fn load(&self) -> io::Result<PersistedState> {
        let (projects, mut sessions, automations, default_cwd) = match self
            .daemon
            .client()
            .request(Uuid::nil(), Uuid::nil(), Command::LoadTaskState)
            .map_err(to_io_error)?
        {
            ResponsePayload::TaskState {
                projects,
                sessions,
                automations,
                default_cwd,
                projectless_root,
            } => {
                waku_protocol::projectless::set_workspace_root(projectless_root);
                (projects, sessions, automations, default_cwd)
            }
            _ => {
                return Err(io::Error::other(
                    "Waku daemon returned an invalid task-state response",
                ));
            }
        };
        restore_task_state_skeletons(&mut sessions);
        *self.remote_default_cwd.lock() = Some(default_cwd);
        let mut state = PersistedState::empty();
        state.projects = projects;
        state.sessions = sessions;
        state.automations = automations;
        let app_settings_missing = !self.app_settings_path.is_file();
        if let Some(settings) = self.read_app_settings()? {
            state.apply_app_settings(settings);
        }
        let app_state = read_app_state_file(&self.app_state_path);
        let app_state_missing = app_state.is_none();
        if let Some(app_state) = app_state {
            state.apply_app_state(app_state);
        }
        state.migrate_loaded();
        if app_settings_missing {
            let _ = self.write_app_settings(&state.app_settings());
        }
        if app_state_missing {
            let _ = self.write_app_state(&state.app_state());
        }
        Ok(state)
    }

    pub fn hydrate(&self, session: &mut AgentSession) -> io::Result<()> {
        if session.detail_loaded {
            return Ok(());
        }
        match hydrate_session(&self.daemon, session.id)? {
            Some(stored) => {
                *session = stored;
                Ok(())
            }
            None => {
                session.detail_loaded = true;
                Ok(())
            }
        }
    }

    pub fn save(&self, state: &mut PersistedState) -> io::Result<()> {
        self.write_app_settings(&state.app_settings())?;
        self.write_app_state(&state.app_state())?;
        if !self.task_state_loaded.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "task state was not loaded; refusing to overwrite daemon data",
            ));
        }
        let automation_changes = state.take_automation_changes();
        if !automation_changes.is_empty() {
            let response = self
                .daemon
                .client()
                .request(
                    Uuid::nil(),
                    Uuid::nil(),
                    Command::ApplyAutomationChanges {
                        changes: automation_changes.clone(),
                    },
                )
                .map_err(to_io_error);
            match response {
                Ok(ResponsePayload::AutomationChangesApplied { automations }) => {
                    state.automations = automations;
                }
                Ok(_) => {
                    state.restore_automation_changes(automation_changes);
                    return Err(io::Error::other(
                        "Waku daemon returned an invalid automation-change response",
                    ));
                }
                Err(error) => {
                    state.restore_automation_changes(automation_changes);
                    return Err(error);
                }
            }
        }
        let dirty_ids = state.dirty_sessions.clone();
        let sessions = state
            .sessions
            .iter()
            .filter(|session| dirty_ids.contains(&session.id))
            .cloned()
            .collect();
        let live_session_ids = state.sessions.iter().map(|session| session.id).collect();
        self.daemon
            .client()
            .notify(
                Uuid::nil(),
                Uuid::nil(),
                Command::SaveTaskState {
                    projects: state.projects.clone(),
                    live_session_ids,
                    sessions,
                },
            )
            .map_err(to_io_error)?;
        state.dirty_sessions.clear();
        Ok(())
    }

    pub fn remove_session(&self, session_id: Uuid) -> io::Result<()> {
        self.daemon
            .client()
            .notify(session_id, Uuid::nil(), Command::RemoveSession)
            .map_err(to_io_error)
    }

    pub fn blob_sweep(&self) -> impl FnOnce() + Send + 'static {
        let daemon = self.daemon.clone();
        move || {
            let _ = daemon
                .client()
                .request(Uuid::nil(), Uuid::nil(), Command::SweepBlobs);
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn read_app_settings(&self) -> io::Result<Option<AppSettings>> {
        let source =
            read_app_settings_source(&self.app_settings_path, &self.legacy_settings_paths)?;
        source
            .map(|(bytes, _)| serde_json::from_slice(&bytes).map_err(to_io_error))
            .transpose()
    }

    fn write_app_settings(&self, settings: &AppSettings) -> io::Result<()> {
        write_json_atomically(&self.app_settings_path, settings)
    }

    fn write_app_state(&self, state: &AppState) -> io::Result<()> {
        write_json_atomically(&self.app_state_path, state)
    }
}

/// Fetches one daemon-owned session. Remote image materialization is kept
/// separate so presentation code can show the transcript before large blobs
/// finish crossing the daemon boundary.
pub fn hydrate_session(
    daemon: &DaemonSupervisor,
    session_id: Uuid,
) -> io::Result<Option<AgentSession>> {
    match daemon
        .client()
        .request(
            Uuid::nil(),
            Uuid::nil(),
            Command::HydrateSession { session_id },
        )
        .map_err(to_io_error)?
    {
        ResponsePayload::Session { session } => Ok(session),
        _ => Err(io::Error::other(
            "Waku daemon returned an invalid session-hydration response",
        )),
    }
}

fn write_json_atomically(path: &Path, value: &impl Serialize) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let data = serde_json::to_vec_pretty(value).map_err(to_io_error)?;
    let temporary = path.with_extension("json.tmp");
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(&temporary)?;
    file.write_all(&data)?;
    file.sync_all()?;
    fs::rename(&temporary, path)?;
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

fn restore_task_state_skeletons(sessions: &mut [AgentSession]) {
    for session in sessions {
        session.detail_loaded = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_settings_paths_are_build_specific() {
        let app_settings_path = default_app_settings_path();
        let legacy_settings_paths = default_legacy_settings_paths();

        #[cfg(debug_assertions)]
        {
            let state_path = StateStore::default_path();
            assert_eq!(app_settings_path, state_path.with_file_name("app.json"));
            assert_eq!(
                legacy_settings_paths,
                [state_path.with_file_name("settings.json")]
            );
        }

        #[cfg(not(debug_assertions))]
        {
            assert_eq!(
                app_settings_path,
                configuration_directory().join("app.json")
            );
            assert_eq!(
                legacy_settings_paths,
                [configuration_directory().join("settings.json")]
            );
        }
    }

    #[test]
    fn automation_deltas_coalesce_only_their_matching_target() {
        let mut state = PersistedState::empty();
        let first = Automation::new("First", ProviderKind::Codex, 1);
        let second = Automation::new("Second", ProviderKind::Codex, 1);
        let mut renamed = first.clone();
        renamed.name = "Renamed".to_owned();

        state.queue_automation_upsert(first);
        state.queue_automation_upsert(second.clone());
        state.queue_automation_upsert(renamed.clone());
        state.queue_automation_removal(second.id, true);

        assert_eq!(
            state.take_automation_changes(),
            vec![
                AutomationChange::Upsert {
                    automation: renamed,
                },
                AutomationChange::Remove {
                    automation_id: second.id,
                    cascade_sessions: true,
                },
            ]
        );
    }

    #[test]
    fn daemon_task_state_becomes_list_only_after_crossing_the_client_boundary() {
        let mut session = AgentSession::new(Uuid::new_v4(), ProviderKind::Codex);
        session.detail_loaded = false;
        assert!(
            session.has_started(),
            "a stored skeleton is visible history"
        );

        let encoded = serde_json::to_vec(&ResponsePayload::TaskState {
            projects: Vec::new(),
            sessions: vec![session],
            default_cwd: PathBuf::from("/daemon/project"),
            projectless_root: None,
            automations: Vec::new(),
        })
        .unwrap();
        let ResponsePayload::TaskState { mut sessions, .. } =
            serde_json::from_slice(&encoded).unwrap()
        else {
            panic!("task state response changed shape");
        };

        assert!(
            !sessions[0].has_started(),
            "serde cannot carry the process-local skeleton marker"
        );
        restore_task_state_skeletons(&mut sessions);
        assert!(!sessions[0].detail_loaded);
        assert!(sessions[0].has_started());
    }
}

/// Reads one daemon-owned binary payload. Clients decide how to present the
/// bytes; this transport layer deliberately creates no client-side files.
pub fn read_remote_reference(
    reference: &str,
    daemon_path: Option<&Path>,
    daemon: &DaemonSupervisor,
) -> Option<Vec<u8>> {
    let command = if waku_protocol::blob::is_reference(reference) {
        Command::ReadBlob {
            reference: reference.to_owned(),
        }
    } else if reference.starts_with(waku_protocol::attachments::ATTACHMENT_SCHEME) {
        let path = daemon_path?;
        Command::ReadAttachment {
            reference: reference.to_owned(),
            path: path.to_owned(),
        }
    } else {
        return None;
    };
    let Ok(ResponsePayload::BlobData { bytes }) =
        daemon.client().request(Uuid::nil(), Uuid::nil(), command)
    else {
        return None;
    };
    Some(bytes)
}

fn normalize_computer_app_grants(grants: &mut Vec<ComputerAppGrant>) {
    let mut seen_bundle_ids = HashSet::new();
    grants.retain(|grant| {
        !grant.bundle_id.trim().is_empty() && seen_bundle_ids.insert(grant.bundle_id.clone())
    });
}

fn to_io_error(error: impl std::fmt::Display) -> io::Error {
    io::Error::other(error.to_string())
}

//! Command-line entrypoint for project/session control (`waku …`).
//!
//! Prefer the running app's control socket when present so in-memory UI state
//! stays authoritative. Fall back to direct SQLite access when the app is not
//! running.

use std::path::PathBuf;
use clap::{Parser, Subcommand};
use uuid::Uuid;

use crate::control::{
    ControlRequest, ControlResponse, parse_provider, parse_uuid, try_request,
};
use crate::model::{AgentSession, Project};
use crate::persistence::{PersistedState, StateStore};
use crate::projectless;

#[derive(Debug, Parser)]
#[command(
    name = "waku",
    about = "Control Waku projects and sessions from the terminal",
    disable_help_subcommand = true
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Create a project from a directory path
    NewProject {
        /// Display name (defaults to the directory name)
        #[arg(long)]
        name: Option<String>,
        /// Project directory (defaults to the current working directory)
        #[arg(long)]
        path: Option<PathBuf>,
    },
    /// List projects
    ListProjects,
    /// Focus a project or session in the running app
    Open {
        /// Project/session id or project name
        target: String,
    },
    /// Create a session
    NewSession {
        /// Project id or name (omit for a projectless task)
        #[arg(long)]
        project: Option<String>,
        /// Provider id: amp, claude, codex, cursor, opencode, grok, pi
        #[arg(long)]
        provider: Option<String>,
        /// Model id for the new session
        #[arg(long)]
        model: Option<String>,
        /// Optional first prompt text stored as the draft title hint
        #[arg(long)]
        prompt: Option<String>,
    },
    /// List sessions
    ListSessions {
        /// Limit to a project id or name
        #[arg(long)]
        project: Option<String>,
    },
    /// Attach an existing session to another project
    LinkSession {
        /// Session id
        #[arg(long)]
        session: String,
        /// Destination project id or name
        #[arg(long)]
        project: String,
    },
}

/// True when argv looks like a CLI invocation rather than launching the GUI.
pub fn wants_cli(args: &[String]) -> bool {
    let Some(first) = args.get(1).map(String::as_str) else {
        return false;
    };
    matches!(
        first,
        "-h" | "--help" | "-V" | "--version" | "help"
            | "new-project"
            | "list-projects"
            | "open"
            | "new-session"
            | "list-sessions"
            | "link-session"
    )
}

pub fn main() -> Result<(), String> {
    run(Cli::parse())
}

fn run(cli: Cli) -> Result<(), String> {
    let request = match cli.command {
        Commands::ListProjects => ControlRequest::ListProjects,
        Commands::NewProject { name, path } => {
            let path = resolve_path(path)?;
            ControlRequest::NewProject { name, path }
        }
        Commands::ListSessions { project } => ControlRequest::ListSessions { project },
        Commands::NewSession {
            project,
            provider,
            model,
            prompt,
        } => ControlRequest::NewSession {
            project,
            provider,
            model,
            prompt,
        },
        Commands::Open { target } => ControlRequest::Open { target },
        Commands::LinkSession { session, project } => ControlRequest::LinkSession { session, project },
    };

    let response = if let Some(response) = try_request(&request) {
        response
    } else {
        match &request {
            ControlRequest::Open { .. } => {
                // Persist selection so the next launch focuses it; focus needs the app.
                let response = run_offline(request)?;
                eprintln!("note: Waku is not running; selection was saved for the next launch");
                response
            }
            _ => run_offline(request)?,
        }
    };

    if !response.ok {
        return Err(response.error.unwrap_or_else(|| "request failed".into()));
    }
    if let Some(data) = response.data {
        println!("{}", serde_json::to_string_pretty(&data).map_err(|e| e.to_string())?);
    }
    Ok(())
}

fn resolve_path(path: Option<PathBuf>) -> Result<PathBuf, String> {
    let path = path.unwrap_or(std::env::current_dir().map_err(|e| e.to_string())?);
    let path = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .map_err(|e| e.to_string())?
            .join(path)
    };
    let path = path.canonicalize().unwrap_or(path);
    if !path.is_dir() {
        return Err(format!("project path is not a directory: {}", path.display()));
    }
    Ok(path)
}

fn run_offline(request: ControlRequest) -> Result<ControlResponse, String> {
    let store = StateStore::new(StateStore::default_path());
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut state = store.load_or_fresh(cwd);
    let data = apply_to_state(&mut state, request)?;
    store
        .save(&mut state)
        .map_err(|error| format!("save state: {error}"))?;
    Ok(ControlResponse::ok_data(data))
}

/// Apply a control request to persisted state. Shared by the offline CLI path
/// and the in-app control handler.
pub fn apply_to_state(
    state: &mut PersistedState,
    request: ControlRequest,
) -> Result<serde_json::Value, String> {
    match request {
        ControlRequest::ListProjects => Ok(serde_json::json!(
            state
                .projects
                .iter()
                .filter(|project| !project.is_projectless())
                .map(project_json)
                .collect::<Vec<_>>()
        )),
        ControlRequest::NewProject { name, path } => {
            if let Some(existing) = state.projects.iter().find(|project| project.path == path) {
                state.selected_project = Some(existing.id);
                return Ok(project_json(existing));
            }
            let mut project = Project::from_path(path);
            if let Some(name) = name.filter(|value| !value.trim().is_empty()) {
                project.name = name;
            }
            let id = project.id;
            let json = project_json(&project);
            state.projects.push(project);
            let session = state.new_session(id, state.last_provider);
            state.selected_project = Some(id);
            state.selected_session = Some(session.id);
            state.push_session(session);
            Ok(json)
        }
        ControlRequest::ListSessions { project } => {
            let project_id = match project {
                Some(value) => Some(resolve_project_id(state, &value)?),
                None => None,
            };
            Ok(serde_json::json!(
                state
                    .sessions
                    .iter()
                    .filter(|session| project_id.is_none_or(|id| session.project_id == id))
                    .map(|session| session_json(state, session))
                    .collect::<Vec<_>>()
            ))
        }
        ControlRequest::NewSession {
            project,
            provider,
            model,
            prompt,
        } => {
            let provider = match provider {
                Some(value) => parse_provider(&value)?,
                None => state.last_provider,
            };
            let project_id = match project {
                Some(value) => resolve_project_id(state, &value)?,
                None => ensure_projectless_project(state)?,
            };
            if let Some(existing) = state
                .sessions
                .iter()
                .find(|session| session.project_id == project_id && !session.has_started())
                .map(|session| session.id)
            {
                state.selected_project = Some(project_id);
                state.selected_session = Some(existing);
                let session = state
                    .sessions
                    .iter()
                    .find(|session| session.id == existing)
                    .expect("session just resolved");
                return Ok(session_json(state, session));
            }
            let mut session = state.new_session(project_id, provider);
            if let Some(model) = model.filter(|value| !value.trim().is_empty()) {
                session.model = Some(model);
            }
            let prompt = prompt.filter(|value| !value.trim().is_empty());
            let id = session.id;
            state.last_provider = provider;
            state.selected_project = Some(project_id);
            state.selected_session = Some(id);
            let json = session_json_with_prompt(state, &session, prompt.as_deref());
            state.push_session(session);
            Ok(json)
        }
        ControlRequest::Open { target } => {
            if let Ok(id) = parse_uuid(&target) {
                if state.sessions.iter().any(|session| session.id == id) {
                    let project_id = state
                        .sessions
                        .iter()
                        .find(|session| session.id == id)
                        .map(|session| session.project_id);
                    state.selected_session = Some(id);
                    if let Some(project_id) = project_id {
                        state.selected_project = Some(project_id);
                    }
                    return Ok(serde_json::json!({ "opened": "session", "id": id }));
                }
                if state.projects.iter().any(|project| project.id == id) {
                    state.selected_project = Some(id);
                    return Ok(serde_json::json!({ "opened": "project", "id": id }));
                }
                return Err(format!("no project or session with id `{target}`"));
            }
            let project_id = resolve_project_id(state, &target)?;
            state.selected_project = Some(project_id);
            Ok(serde_json::json!({ "opened": "project", "id": project_id }))
        }
        ControlRequest::LinkSession { session, project } => {
            let session_id = parse_uuid(&session)?;
            let project_id = resolve_project_id(state, &project)?;
            let Some(session) = state.session_mut(session_id) else {
                return Err(format!("session not found: {session}"));
            };
            session.project_id = project_id;
            session.updated_at = crate::model::unix_time();
            state.selected_session = Some(session_id);
            state.selected_project = Some(project_id);
            let session = state
                .sessions
                .iter()
                .find(|session| session.id == session_id)
                .expect("session just linked");
            Ok(session_json(state, session))
        }
    }
}

fn ensure_projectless_project(state: &mut PersistedState) -> Result<Uuid, String> {
    if let Some(project) = state.projects.iter().find(|project| {
        project.is_projectless() && !projectless::is_legacy_root_path(&project.path)
    }) {
        return Ok(project.id);
    }
    let workspace = projectless::create_workspace(None).map_err(|error| error.to_string())?;
    let mut project = Project::from_path(workspace.cwd);
    project.name = Project::PROJECTLESS_NAME.to_owned();
    let id = project.id;
    state.projects.push(project);
    Ok(id)
}

fn resolve_project_id(state: &PersistedState, value: &str) -> Result<Uuid, String> {
    if let Ok(id) = parse_uuid(value) {
        if state.projects.iter().any(|project| project.id == id) {
            return Ok(id);
        }
        return Err(format!("project not found: {value}"));
    }
    let matches = state
        .projects
        .iter()
        .filter(|project| {
            !project.is_projectless() && project.name.eq_ignore_ascii_case(value.trim())
        })
        .map(|project| project.id)
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [id] => Ok(*id),
        [] => Err(format!("project not found: {value}")),
        _ => Err(format!(
            "ambiguous project name `{value}`; use a project id"
        )),
    }
}

fn project_json(project: &Project) -> serde_json::Value {
    serde_json::json!({
        "id": project.id,
        "name": project.name,
        "path": project.path,
        "created_at": project.created_at,
    })
}

fn session_json(state: &PersistedState, session: &AgentSession) -> serde_json::Value {
    session_json_with_prompt(state, session, None)
}

fn session_json_with_prompt(
    state: &PersistedState,
    session: &AgentSession,
    prompt: Option<&str>,
) -> serde_json::Value {
    let project_name = state
        .projects
        .iter()
        .find(|project| project.id == session.project_id)
        .map(|project| project.display_name())
        .unwrap_or_else(|| session.project_id.to_string());
    let mut value = serde_json::json!({
        "id": session.id,
        "title": session.title,
        "project_id": session.project_id,
        "project_name": project_name,
        "provider": session.provider.id(),
        "model": session.model,
        "created_at": session.created_at,
        "updated_at": session.updated_at,
        "started": session.has_started(),
    });
    if let Some(prompt) = prompt {
        value["prompt"] = serde_json::json!(prompt);
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::ControlRequest;

    #[test]
    fn new_project_and_list_round_trip() {
        let mut state = PersistedState::empty();
        let path = std::env::temp_dir().join(format!("waku-cli-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&path).unwrap();
        let created = apply_to_state(
            &mut state,
            ControlRequest::NewProject {
                name: Some("Demo".into()),
                path: path.clone(),
            },
        )
        .unwrap();
        assert_eq!(created["name"], "Demo");
        let listed = apply_to_state(&mut state, ControlRequest::ListProjects).unwrap();
        assert_eq!(listed.as_array().unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn link_session_moves_project() {
        let mut state = PersistedState::empty();
        let a = std::env::temp_dir().join(format!("waku-cli-a-{}", Uuid::new_v4()));
        let b = std::env::temp_dir().join(format!("waku-cli-b-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        apply_to_state(
            &mut state,
            ControlRequest::NewProject {
                name: Some("A".into()),
                path: a.clone(),
            },
        )
        .unwrap();
        apply_to_state(
            &mut state,
            ControlRequest::NewProject {
                name: Some("B".into()),
                path: b.clone(),
            },
        )
        .unwrap();
        let session_id = state.sessions[0].id;
        let project_b = state.projects.iter().find(|p| p.name == "B").unwrap().id;
        apply_to_state(
            &mut state,
            ControlRequest::LinkSession {
                session: session_id.to_string(),
                project: project_b.to_string(),
            },
        )
        .unwrap();
        assert_eq!(state.sessions[0].project_id, project_b);
        let _ = std::fs::remove_dir_all(a);
        let _ = std::fs::remove_dir_all(b);
    }

    #[test]
    fn wants_cli_detects_subcommands() {
        assert!(wants_cli(&[
            "waku".into(),
            "list-projects".into()
        ]));
        assert!(!wants_cli(&["waku".into()]));
    }
}

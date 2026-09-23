//! Devin CLI session-title lookup.
//!
//! Devin generates a descriptive title itself and stores it on the `sessions`
//! row in `~/.local/share/devin/cli/sessions.db` (XDG data on Linux, the
//! equivalent `dirs::data_local_dir` location elsewhere). ACP replays that
//! title as `session_info_update` on `session/load`, but a live first turn
//! never pushes one: the generator runs after `session/prompt` returns, and
//! the only durable copy is this file.
//!
//! Catalog and history stay on ACP `session/list` / `session/load`. This
//! module exists so the live driver can poll the title without spawning a
//! second `devin` process against a locked session.

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde_json::Value;

pub(crate) fn generated_title(
    session_id: &str,
    placeholder: Option<&str>,
) -> anyhow::Result<Option<String>> {
    let Some(path) = sessions_db_path() else {
        return Ok(None);
    };
    generated_title_from(&path, session_id, placeholder)
}

pub(crate) fn generated_title_from(
    db_path: &Path,
    session_id: &str,
    placeholder: Option<&str>,
) -> anyhow::Result<Option<String>> {
    if !valid_session_id(session_id) {
        return Ok(None);
    }
    let connection = Connection::open_with_flags(
        db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("could not read {}", db_path.display()))?;
    let title: Option<String> = connection
        .query_row(
            "SELECT title FROM sessions WHERE id = ?1",
            [session_id],
            |row| row.get(0),
        )
        .optional()
        .context("Devin's session store is not readable")?;
    Ok(title.and_then(|title| distinct_title(&title, placeholder)))
}

/// True when `title` is empty, Devin's "Untitled" stub, or the first prompt
/// Devin writes before generation finishes. Those are misses, not titles:
/// latching them would hide the generated name that lands a moment later.
pub(crate) fn is_placeholder_title(title: &str, prompt: Option<&str>) -> bool {
    distinct_title(title, prompt).is_none()
}

pub(crate) fn title_from_notification(method: &str, params: &Value) -> Option<String> {
    if !is_title_notification(method) {
        return None;
    }
    ["title", "name"]
        .into_iter()
        .find_map(|key| {
            params.get(key).and_then(Value::as_str).or_else(|| {
                params
                    .get("_meta")
                    .and_then(|meta| meta.get(key))
                    .and_then(Value::as_str)
            })
        })
        .or_else(|| params.get("cognition.ai/title").and_then(Value::as_str))
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .map(str::to_owned)
}

fn distinct_title(title: &str, prompt: Option<&str>) -> Option<String> {
    let trimmed = title.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("untitled") {
        return None;
    }
    let normalized = normalize_title(trimmed);
    if normalized.is_empty() {
        return None;
    }
    if let Some(prompt) = prompt.map(str::trim).filter(|prompt| !prompt.is_empty()) {
        if normalized.eq_ignore_ascii_case(&normalize_title(prompt)) {
            return None;
        }
        if let Some(line) = prompt.lines().next() {
            let line = normalize_title(line);
            if !line.is_empty() && normalized.eq_ignore_ascii_case(&line) {
                return None;
            }
        }
        let fallback = fallback_title_from_prompt(prompt);
        if !fallback.is_empty() && normalized.eq_ignore_ascii_case(&fallback) {
            return None;
        }
    }
    Some(trimmed.to_owned())
}

fn fallback_title_from_prompt(prompt: &str) -> String {
    let mut title = prompt
        .split_whitespace()
        .take(7)
        .collect::<Vec<_>>()
        .join(" ");
    if title.chars().count() > 54 {
        title = format!("{}…", title.chars().take(53).collect::<String>());
    }
    title
}

fn normalize_title(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn valid_session_id(session_id: &str) -> bool {
    !session_id.is_empty()
        && !session_id.contains('/')
        && !session_id.contains('\\')
        && !session_id.contains('\0')
}

fn is_title_notification(method: &str) -> bool {
    let method = method.trim_start_matches('_');
    method == "cognition.ai/session/rename"
        || method == "cognition.ai/title"
        || method.ends_with("/session/rename")
}

fn sessions_db_path() -> Option<PathBuf> {
    let dir = dirs::data_local_dir()?.join("devin").join("cli");
    for name in ["sessions.db", "cli_sessions.db"] {
        let path = dir.join(name);
        if path.is_file() {
            return Some(path);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn write_sessions_db(path: &Path, rows: &[(&str, Option<&str>)]) {
        let connection = Connection::open(path).unwrap();
        connection
            .execute(
                "CREATE TABLE sessions (
                    id TEXT PRIMARY KEY,
                    working_directory TEXT NOT NULL DEFAULT '',
                    backend_type TEXT NOT NULL DEFAULT '',
                    model TEXT NOT NULL DEFAULT '',
                    agent_mode TEXT NOT NULL DEFAULT '',
                    created_at INTEGER NOT NULL DEFAULT 0,
                    last_activity_at INTEGER NOT NULL DEFAULT 0,
                    title TEXT
                )",
                [],
            )
            .unwrap();
        for (id, title) in rows {
            connection
                .execute(
                    "INSERT INTO sessions (id, title) VALUES (?1, ?2)",
                    rusqlite::params![id, title],
                )
                .unwrap();
        }
    }

    #[test]
    fn skips_the_first_prompt_placeholder_and_returns_a_generated_title() {
        let root = std::env::temp_dir().join(format!("waku-devin-title-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let db = root.join("sessions.db");
        write_sessions_db(
            &db,
            &[
                ("festive-apparel", Some("hi")),
                ("brave-actress", Some("  Fix the sidebar row builder  ")),
                ("untitled-one", Some("Untitled")),
                ("empty-one", Some("   ")),
            ],
        );

        assert_eq!(
            generated_title_from(&db, "festive-apparel", Some("hi"))
                .unwrap()
                .as_deref(),
            None
        );
        assert_eq!(
            generated_title_from(&db, "brave-actress", Some("hi"))
                .unwrap()
                .as_deref(),
            Some("Fix the sidebar row builder")
        );
        assert_eq!(
            generated_title_from(&db, "untitled-one", None)
                .unwrap()
                .as_deref(),
            None
        );
        assert_eq!(
            generated_title_from(&db, "empty-one", None)
                .unwrap()
                .as_deref(),
            None
        );
        assert_eq!(
            generated_title_from(&db, "missing", None)
                .unwrap()
                .as_deref(),
            None
        );
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn treats_waku_fallback_text_as_a_placeholder() {
        let prompt = "build a really polished local agent interface for rust and ship it";
        let fallback = fallback_title_from_prompt(prompt);
        assert!(is_placeholder_title(&fallback, Some(prompt)));
        assert!(is_placeholder_title(prompt, Some(prompt)));
        assert!(!is_placeholder_title(
            "Polish the native agent interface",
            Some(prompt)
        ));
    }

    #[test]
    fn reads_a_title_from_devins_rename_notification() {
        assert_eq!(
            title_from_notification(
                "_cognition.ai/session/rename",
                &json!({"title": "  Name the session  "}),
            )
            .as_deref(),
            Some("Name the session")
        );
        assert_eq!(
            title_from_notification(
                "cognition.ai/session/rename",
                &json!({"_meta": {"title": "From meta"}}),
            )
            .as_deref(),
            Some("From meta")
        );
        assert!(
            title_from_notification(
                "_cognition.ai/mcp/serversChanged",
                &json!({"title": "not a title event"}),
            )
            .is_none()
        );
    }
}

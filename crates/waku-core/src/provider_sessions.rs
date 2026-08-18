//! Sessions Claude Code recorded outside Waku, for `/resume`.
//!
//! The CLI still holds each conversation, so a transcript is read only for the
//! identifier to resume by, the `cwd` placing it in the open project, and a
//! title. Reading one for its content is [`crate::claude_session`]'s job.
//!
//! Every function here reads the filesystem: background executor only.

use std::path::{Path, PathBuf};

use serde_json::Value;
use waku_protocol::provider_session::{ImportedSessionRecord, ResumableSession};

use crate::model::{ProviderKind, ProviderResumeCursor};

/// Sessions offered for one project. Past this the picker is a list to read
/// rather than scan.
const LIST_CAP: usize = 20;
/// Transcripts whose head is read per scan. Past this the rest are older than
/// anyone is looking for.
const SCAN_CAP: usize = 250;
/// Bytes read from the start of a transcript, where its `cwd` sits. The rest
/// of the file can be megabytes of tool output none of this wants.
const HEAD_BYTES: u64 = 32 * 1024;
/// Bytes read back from the end of a transcript, looking for its title. One
/// turn can write hundreds of kilobytes, so the window grows until it hits one.
const TAIL_WINDOWS: [u64; 3] = [64 * 1024, 512 * 1024, 2 * 1024 * 1024];

pub fn list(provider: ProviderKind, project_root: &Path) -> Vec<ResumableSession> {
    match provider {
        ProviderKind::Claude => list_claude_sessions(project_root),
        _ => Vec::new(),
    }
}

pub fn import(cursor: ProviderResumeCursor) -> anyhow::Result<Vec<ImportedSessionRecord>> {
    match cursor {
        ProviderResumeCursor::Claude { session_id, .. } => {
            crate::claude_session::imported_transcript(&session_id)
        }
        cursor => anyhow::bail!(
            "{} sessions cannot be imported",
            cursor.provider().display_name()
        ),
    }
}

/// Where a provider keeps its transcripts, honouring the CLI's own override.
pub fn transcript_root(provider: ProviderKind) -> Option<PathBuf> {
    match provider {
        ProviderKind::Claude => {
            crate::claude_session::claude_config_dir().map(|dir| dir.join("projects"))
        }
        ProviderKind::Codex => std::env::var_os("CODEX_HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .or_else(|| dirs::home_dir().map(|home| home.join(".codex")))
            .map(|dir| dir.join("sessions")),
        _ => None,
    }
}

/// The sessions Claude Code recorded for `project_root`, newest first.
///
/// The recorded `cwd` must match exactly: the CLI resumes into the directory
/// Waku launches it in, so a subdirectory session would move the agent.
///
/// Blocking, so background executor only.
pub fn list_claude_sessions(project_root: &Path) -> Vec<ResumableSession> {
    let Some(root) = transcript_root(ProviderKind::Claude) else {
        return Vec::new();
    };
    let mut candidates = claude_candidates(&root, project_root);
    candidates.sort_by_key(|(_, _, modified_ms)| std::cmp::Reverse(*modified_ms));
    candidates.truncate(SCAN_CAP);

    let mut sessions = Vec::new();
    for (path, length, modified_ms) in candidates {
        let Some(head) = read_head(&path) else {
            continue;
        };
        let Some(cwd) = recorded_cwd(&head) else {
            continue;
        };
        if Path::new(&cwd) != project_root {
            continue;
        }
        let Some(id) = path
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
        else {
            continue;
        };
        sessions.push(ResumableSession {
            label: session_label(&head, &path, length).unwrap_or_else(|| id.clone()),
            cursor: ProviderResumeCursor::from_session_id(ProviderKind::Claude, id),
            modified_ms,
        });
        if sessions.len() == LIST_CAP {
            break;
        }
    }
    sessions
}

/// Claude Code names each project directory after its working directory, one
/// dash per non-alphanumeric character. Only a prefix, since `cwd` decides.
fn claude_candidates(root: &Path, project_root: &Path) -> Vec<(PathBuf, u64, i64)> {
    let prefix = project_root
        .to_string_lossy()
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                '-'
            }
        })
        .collect::<String>();
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|entry| entry.file_name().to_string_lossy().starts_with(&prefix))
        .flat_map(|entry| {
            let mut found = Vec::new();
            // Shallow: a subagent's transcript nests under
            // `<session>/subagents/` and is work, not a session to resume.
            list_transcripts(&entry.path(), i64::MIN, false, &mut found);
            found
        })
        .collect()
}

/// Lists `.jsonl` transcripts in `root`, and below it when `recurse`, modified
/// at or after `since_ms`, as `(path, length, modified)`. Entries that error
/// are skipped, since files rotate mid-walk. Returns how many `since_ms` cut.
pub fn list_transcripts(
    root: &Path,
    since_ms: i64,
    recurse: bool,
    found: &mut Vec<(PathBuf, u64, i64)>,
) -> usize {
    let mut skipped = 0;
    let Ok(entries) = std::fs::read_dir(root) else {
        return 0;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            if recurse {
                skipped += list_transcripts(&path, since_ms, recurse, found);
            }
            continue;
        }
        if path.extension().and_then(|extension| extension.to_str()) != Some("jsonl") {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        let mtime_ms = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|elapsed| elapsed.as_millis() as i64)
            .unwrap_or(0);
        if mtime_ms < since_ms {
            skipped += 1;
            continue;
        }
        found.push((path, metadata.len(), mtime_ms));
    }
    skipped
}

/// The transcript's opening records. A truncated read cuts the last line
/// mid-JSON, which then fails to parse and is skipped.
fn read_head(path: &Path) -> Option<Vec<String>> {
    use std::io::Read as _;
    let mut buffer = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(HEAD_BYTES)
        .read_to_end(&mut buffer)
        .ok()?;
    Some(split_records(&buffer, false))
}

/// The transcript's last `window` bytes as records. A seeked read opens on a
/// partial record, so that line is dropped.
fn read_tail(path: &Path, window: u64, length: u64) -> Vec<String> {
    use std::io::{Read as _, Seek as _, SeekFrom};
    let read = || -> Option<Vec<String>> {
        let mut file = std::fs::File::open(path).ok()?;
        let start = length.saturating_sub(window);
        file.seek(SeekFrom::Start(start)).ok()?;
        let mut buffer = Vec::new();
        file.read_to_end(&mut buffer).ok()?;
        Some(split_records(&buffer, start > 0))
    };
    read().unwrap_or_default()
}

fn split_records(buffer: &[u8], drop_first: bool) -> Vec<String> {
    String::from_utf8_lossy(buffer)
        .lines()
        .skip(usize::from(drop_first))
        .map(str::to_owned)
        .collect()
}

/// The working directory the session ran in, as its own records report it.
fn recorded_cwd(records: &[String]) -> Option<String> {
    records
        .iter()
        .filter(|line| line.contains("\"cwd\""))
        .find_map(|line| {
            serde_json::from_str::<Value>(line)
                .ok()?
                .get("cwd")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
}

/// How the picker names one session: the newest title reachable, since one is
/// appended on every rename, else the newest typed prompt.
fn session_label(head: &[String], path: &Path, length: u64) -> Option<String> {
    for (index, window) in TAIL_WINDOWS.into_iter().enumerate() {
        let tail = read_tail(path, window, length);
        if index == 0
            && let title @ Some(_) = newest_title(&tail).or_else(|| newest_title(head))
        {
            return title;
        }
        if let Some(prompt) = latest_prompt(&tail) {
            return Some(prompt);
        }
        if window >= length {
            break;
        }
    }
    None
}

fn newest_title(records: &[String]) -> Option<String> {
    // Titles are rare in tens of kilobytes, so look for the field name before
    // parsing a line at all.
    records
        .iter()
        .rev()
        .filter(|line| line.contains("Title"))
        .find_map(|line| {
            crate::claude_session::claude_title(&serde_json::from_str::<Value>(line).ok()?)
        })
}

/// The newest typed prompt in `records`. Newest rather than first, since a
/// session opens by replaying instruction files through the user role.
fn latest_prompt(records: &[String]) -> Option<String> {
    records
        .iter()
        .rev()
        .filter(|line| line.contains("\"user\""))
        .find_map(|line| {
            let record = serde_json::from_str::<Value>(line).ok()?;
            let entry = record.as_object()?;
            (entry.get("type").and_then(Value::as_str) == Some("user"))
                .then(|| crate::claude_session::typed_prompt(entry))
                .flatten()
                .as_deref()
                .and_then(first_prompt_line)
        })
}

/// The first line of a typed prompt, as a picker label. A replayed instruction
/// file opens with a heading and would label every session alike.
fn first_prompt_line(prompt: &str) -> Option<String> {
    if prompt.starts_with('#') {
        return None;
    }
    let line = prompt.lines().next()?.trim();
    (!line.is_empty()).then(|| line.chars().take(120).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_yield_their_cwd_and_newest_typed_prompt() {
        let records = [
            r#"{"type":"mode","mode":"normal"}"#.to_owned(),
            r#"{"type":"user","message":{"role":"user","content":"fix the parser\nsecond line"},"cwd":"/repo"}"#.to_owned(),
            r#"{"type":"user","message":{"role":"user","content":"<command-name>/init</command-name>"},"cwd":"/repo"}"#.to_owned(),
        ];
        assert_eq!(recorded_cwd(&records).as_deref(), Some("/repo"));
        // The replayed command record is passed over for the real prompt.
        assert_eq!(latest_prompt(&records).as_deref(), Some("fix the parser"));
    }

    #[test]
    fn a_claude_title_is_taken_from_the_newest_title_record() {
        let records = [
            r#"{"type":"ai-title","aiTitle":"Generated title"}"#.to_owned(),
            r#"{"type":"custom-title","customTitle":"Renamed by hand"}"#.to_owned(),
            r#"{"type":"user","message":{"role":"user","content":"a later prompt"}}"#.to_owned(),
        ];
        assert_eq!(newest_title(&records).as_deref(), Some("Renamed by hand"));
        // An untitled session is what sends the label to its newest prompt.
        assert_eq!(newest_title(&records[2..]), None);
    }

    #[test]
    fn a_transcript_without_a_recorded_cwd_cannot_be_attributed() {
        let records = [r#"{"type":"mode","mode":"normal"}"#.to_owned()];
        assert_eq!(recorded_cwd(&records), None);
    }

    #[test]
    fn a_seeked_read_drops_its_partial_opening_record() {
        let buffer = b"ent\"}\n{\"type\":\"user\"}\n";
        assert_eq!(split_records(buffer, true), vec!["{\"type\":\"user\"}"]);
        assert_eq!(split_records(buffer, false).len(), 2);
    }

    #[test]
    fn the_widest_tail_window_is_checked_for_a_prompt() {
        let path = std::env::temp_dir().join(format!(
            "waku-provider-session-tail-{}.jsonl",
            std::process::id()
        ));
        let mut transcript =
            r#"{"type":"user","message":{"role":"user","content":"older prompt"}}"#
                .to_owned();
        transcript.push('\n');
        transcript.push_str(&"x\n".repeat(300_000));
        std::fs::write(&path, &transcript).unwrap();

        assert_eq!(
            session_label(&[], &path, transcript.len() as u64).as_deref(),
            Some("older prompt")
        );
        std::fs::remove_file(path).ok();
    }
}

//! Kimi Code native-session helpers.
//!
//! Kimi reports an upstream provider failure — an expired plan, a rejected
//! key, a quota wall — only in its own per-session wire log. Over ACP the same
//! turn simply returns `end_turn` carrying no content at all, with nothing on
//! stderr and no JSON-RPC error, so a client that trusts the protocol shows an
//! empty answer and calls it a success. Reading the wire log is what lets Waku
//! name the real cause instead.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::Value;

/// Kimi appends the turn record just after the ACP response returns — about
/// 50ms in practice — so the lookup has to outlast that gap without stalling a
/// turn that simply has nothing to report.
const TURN_RECORD_WAIT: Duration = Duration::from_millis(1000);
const TURN_RECORD_POLL: Duration = Duration::from_millis(25);

/// How much of the tail to scan. A turn record is written last, so the end of
/// the file is the only part worth reading, and a turn's own transcript can be
/// arbitrarily large.
const WIRE_TAIL_BYTES: u64 = 128 * 1024;

/// Kimi's data directory, matching the CLI's own `KIMI_CODE_HOME` override.
pub fn session_home() -> Option<PathBuf> {
    match std::env::var_os("KIMI_CODE_HOME") {
        Some(home) if !home.is_empty() => Some(PathBuf::from(home)),
        _ => dirs::home_dir().map(|home| home.join(".kimi-code")),
    }
}

/// Where the current length of a session's wire log is read, so a later
/// failure lookup can ignore everything an earlier turn already wrote.
pub fn wire_offset(session_id: &str) -> u64 {
    session_home()
        .and_then(|home| wire_log(&home, session_id))
        .and_then(|path| path.metadata().ok())
        .map_or(0, |metadata| metadata.len())
}

/// The provider error that ended this turn, or `None` when the turn did not
/// record a failure. Polls briefly because the record lands just after the ACP
/// response; call it only for a turn that produced nothing, so a healthy turn
/// never pays the wait.
pub fn turn_failure(session_id: &str, from_offset: u64) -> Option<String> {
    let home = session_home()?;
    turn_failure_in(&home, session_id, from_offset, TURN_RECORD_WAIT)
}

fn turn_failure_in(
    home: &Path,
    session_id: &str,
    from_offset: u64,
    wait: Duration,
) -> Option<String> {
    let deadline = Instant::now() + wait;
    loop {
        if let Some(path) = wire_log(home, session_id)
            && let Some(failure) = recorded_failure(&path, from_offset)
        {
            return Some(failure);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(TURN_RECORD_POLL);
    }
}

/// Sessions are filed under a per-workspace directory whose name carries a
/// hash Waku cannot reproduce, so the session id is matched by scanning.
fn wire_log(home: &Path, session_id: &str) -> Option<PathBuf> {
    if session_id.is_empty() || session_id.contains(std::path::MAIN_SEPARATOR) {
        return None;
    }
    std::fs::read_dir(home.join("sessions"))
        .ok()?
        .flatten()
        .find_map(|entry| {
            let candidate = entry
                .path()
                .join(session_id)
                .join("agents")
                .join("main")
                .join("wire.jsonl");
            candidate.is_file().then_some(candidate)
        })
}

fn recorded_failure(path: &Path, from_offset: u64) -> Option<String> {
    parse_turn_failure(&read_tail(path, from_offset)?)
}

fn read_tail(path: &Path, from_offset: u64) -> Option<String> {
    let mut file = std::fs::File::open(path).ok()?;
    let length = file.metadata().ok()?.len();
    if length <= from_offset {
        return None;
    }
    let start = from_offset.max(length.saturating_sub(WIRE_TAIL_BYTES));
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut tail = Vec::with_capacity((length - start) as usize);
    file.read_to_end(&mut tail).ok()?;
    Some(String::from_utf8_lossy(&tail).into_owned())
}

/// The last `turn.ended` record in the scanned region, when it failed. A
/// partially flushed final line is simply skipped and retried by the caller.
fn parse_turn_failure(tail: &str) -> Option<String> {
    tail.lines()
        .rev()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|record| record.get("type").and_then(Value::as_str) == Some("turn.ended"))
        .filter(|record| record.get("reason").and_then(Value::as_str) == Some("failed"))
        .and_then(|record| {
            let error = record.get("error")?;
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|message| !message.is_empty())?;
            Some(message.to_owned())
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_provider_error_off_a_failed_turn() {
        let tail = concat!(
            r#"{"type":"turn.prompt","turnId":0}"#,
            "\n",
            r#"{"type":"turn.ended","turnId":0,"reason":"failed","error":{"code":"provider.api_error","message":"402 membership inactive","retryable":false}}"#,
            "\n"
        );

        assert_eq!(
            parse_turn_failure(tail).as_deref(),
            Some("402 membership inactive")
        );
    }

    #[test]
    fn ignores_a_turn_that_did_not_fail() {
        let tail = concat!(
            r#"{"type":"turn.ended","turnId":0,"reason":"failed","error":{"message":"stale"}}"#,
            "\n",
            r#"{"type":"turn.ended","turnId":1,"reason":"completed"}"#,
            "\n"
        );

        assert!(parse_turn_failure(tail).is_none());
    }

    #[test]
    fn waits_out_a_record_that_has_not_been_flushed_yet() {
        // The last line is still being written, so it does not parse. The
        // caller must not read the previous turn's failure as this turn's.
        let tail = concat!(
            r#"{"type":"turn.ended","turnId":0,"reason":"completed"}"#,
            "\n",
            r#"{"type":"turn.ended","turnId":1,"reas"#
        );

        assert!(parse_turn_failure(tail).is_none());
    }

    #[test]
    fn a_missing_session_never_blocks_past_its_deadline() {
        let started = Instant::now();
        let failure = turn_failure_in(
            Path::new("/nonexistent-kimi-home"),
            "session_missing",
            0,
            Duration::from_millis(80),
        );

        assert!(failure.is_none());
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}

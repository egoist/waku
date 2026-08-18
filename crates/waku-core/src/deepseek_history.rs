//! Typed, allocation-lean parsing of DeepSeek Harness `session.history`
//! responses.
//!
//! The HTTP body of a history response is one RPC envelope
//! (`{type, rpcId, result: {ok, value}}`) whose `value` is a history page
//! (`{events: [{event, view?}], hasMore, projections?}`). Parsing that whole
//! envelope into `serde_json::Value` — what the driver did before — builds a
//! full value tree (millions of allocations for a large page) even though the
//! driver reads only a handful of fields per event.
//!
//! Instead the page is deserialized into typed structs mirroring the wire
//! contract in deepseek-harness:
//! `packages/host/apiproxy/src/api/sessions.schema.ts` (envelope/page/entry),
//! `packages/core/session/src/types.ts` (the `SessionEvent` map), and
//! `packages/llm/llm/src/types.ts` + `message.ts` (chunks and content blocks).
//! The wire contract marks event `data` as wide (`unknown`), so the typed
//! `data` struct here is a tolerant union of exactly the fields the driver
//! reads; unknown fields and unknown event types are ignored.
//!
//! Escaped-string fields (`chunk.text`, `arguments`, `title`, ...) are
//! captured as borrowed `&RawValue`. serde.rs cannot hand out a borrowed
//! decoded `&str` for an escaped string (the escape work needs an owned
//! buffer, so such fields can only deserialize into `String`), but
//! `&RawValue` is zero-copy: it borrows the raw escaped slice straight out of
//! the input bytes, with no per-field allocation during the parse. Text
//! fields are decoded individually when events are converted — at message
//! level a page carries only a few hundred texts, so there is no need for a
//! deduplicated decode pool.


use anyhow::{anyhow, Context as _};
use serde::Deserialize;
use serde_json::value::RawValue;
use serde_json::Value;

// ---------------------------------------------------------------------------
// Wire schema (borrowed: `&RawValue` text fields are zero-copy)
// ---------------------------------------------------------------------------

/// Full HTTP body of one RPC response: `{type, rpcId, result: {ok, value}}`.
#[derive(Deserialize)]
#[serde(bound(deserialize = "'de: 'a"))]
struct Envelope<'a> {
    #[serde(rename = "type")]
    kind: &'a str,
    #[serde(rename = "rpcId")]
    rpc_id: &'a str,
    result: EnvelopeResult<'a>,
}

#[derive(Deserialize)]
#[serde(bound(deserialize = "'de: 'a"))]
struct EnvelopeResult<'a> {
    ok: bool,
    value: &'a RawValue,
}

/// `session.history` response value: `{events, hasMore, projections?}`.
#[derive(Deserialize)]
#[serde(bound(deserialize = "'de: 'a"))]
pub(crate) struct HistoryPage<'a> {
    pub(crate) events: Vec<HistoryEntry<'a>>,
    #[serde(rename = "hasMore")]
    pub(crate) has_more: bool,
    #[serde(default)]
    pub(crate) projections: Option<ProjectionsBlock<'a>>,
}

#[derive(Deserialize)]
#[serde(bound(deserialize = "'de: 'a"))]
pub(crate) struct ProjectionsBlock<'a> {
    pub(crate) values: &'a RawValue,
}

/// One history item: `{event, view?}`. The host-computed tool `view` stays
/// raw; it is parsed to `Value` only for tool events that present it.
#[derive(Deserialize)]
#[serde(bound(deserialize = "'de: 'a"))]
pub(crate) struct HistoryEntry<'a> {
    #[serde(default)]
    pub(crate) event: Option<HistoryEventWire<'a>>,
    #[serde(default)]
    pub(crate) view: Option<&'a RawValue>,
}

/// SessionEvent envelope: `{type, seq, time, data, ...}`. Only the fields the
/// driver reads are captured; `time`, `sourceEventSeqs`, `surfaceOp` and
/// `ignorable` are skipped.
#[derive(Deserialize)]
#[serde(bound(deserialize = "'de: 'a"))]
pub(crate) struct HistoryEventWire<'a> {
    #[serde(rename = "type")]
    pub(crate) kind: &'a str,
    pub(crate) seq: u64,
    pub(crate) data: EventDataWire<'a>,
}

/// Tolerant union of the `data` fields the driver reads, per the event map in
/// `packages/core/session/src/types.ts` (SessionEventMap). Unknown fields and
/// unknown event types are ignored by construction. Text-bearing fields are
/// `&RawValue` so they stay undecoded (and unallocated) until the batch pass.
#[derive(Deserialize, Default)]
#[serde(bound(deserialize = "'de: 'a"))]
pub(crate) struct EventDataWire<'a> {
    pub(crate) turn: Option<u64>,
    pub(crate) step: Option<u64>,
    /// Only the delta-bearing fields are kept: they feed orphan-step text
    /// aggregation for aborted turns.
    pub(crate) chunk: Option<StreamChunkWire<'a>>,
    /// `data.message` — raw, because its shape differs per event type: a
    /// content-block list for `assistant/message`/`user/message`, a nested
    /// tool-result block for `tool/result`.
    #[serde(default)]
    pub(crate) message: Option<&'a RawValue>,
    #[serde(rename = "callId")]
    pub(crate) call_id: Option<&'a str>,
    pub(crate) name: Option<&'a str>,
    /// `data.arguments` — raw JSON string as produced by the model.
    #[serde(default)]
    pub(crate) arguments: Option<&'a RawValue>,
    /// `turn/end` carries `{kind, error?}`; `request/header` carries a plain
    /// string (`'initial' | 'resume' | 'change'`). Accept both.
    #[serde(default)]
    pub(crate) reason: Option<ReasonWire<'a>>,
    #[serde(default)]
    pub(crate) todos: Option<&'a RawValue>,
    #[serde(rename = "contextWindow")]
    pub(crate) context_window: Option<u64>,
    #[serde(default)]
    pub(crate) title: Option<&'a RawValue>,
    /// `data.usage` on `assistant/message` events.
    #[serde(default)]
    pub(crate) usage: Option<&'a RawValue>,
    /// `data.error` on `tool/result` events (internal failure identity).
    #[serde(default)]
    pub(crate) error: Option<&'a RawValue>,
}

/// StreamChunk: only the delta fields needed to aggregate aborted-step text.
#[derive(Deserialize, Default)]
#[serde(bound(deserialize = "'de: 'a"))]
pub(crate) struct StreamChunkWire<'a> {
    #[serde(rename = "type")]
    pub(crate) kind: &'a str,
    #[serde(default)]
    pub(crate) text: Option<&'a RawValue>,
}

/// `data.reason`: the `turn/end` object or the `request/header` string.
#[derive(Deserialize)]
#[serde(untagged)]
pub(crate) enum ReasonWire<'a> {
    Obj(TurnEndReasonWire<'a>),
    /// `request/header` events carry a plain string reason; the untagged
    /// fallback keeps them from failing the whole page parse. Never read.
    #[allow(dead_code)]
    Str(&'a str),
}

#[derive(Deserialize, Default)]
#[serde(bound(deserialize = "'de: 'a"))]
pub(crate) struct TurnEndReasonWire<'a> {
    #[serde(default)]
    pub(crate) kind: Option<&'a str>,
    #[serde(default)]
    pub(crate) error: Option<LlmFailureWire<'a>>,
}

#[derive(Deserialize, Default)]
#[serde(bound(deserialize = "'de: 'a"))]
pub(crate) struct LlmFailureWire<'a> {
    #[serde(default)]
    pub(crate) message: Option<&'a RawValue>,
}

// ---------------------------------------------------------------------------
// Owned, pool-referencing event model consumed by the driver
// ---------------------------------------------------------------------------

/// Why a `turn/end` event ended (mirrors `TurnEndReason`).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum TurnEndKind {
    Completed,
    Aborted,
    Blocked,
    Error,
    MaxTokens,
    Interrupted,
    Other,
}

/// Token accounting for one model call (mirrors `TokenUsage`).
#[derive(Clone, Copy, Default)]
pub(crate) struct TokenUsage {
    pub(crate) input_tokens: u64,
    pub(crate) output_tokens: u64,
    pub(crate) cache_read_tokens: u64,
    pub(crate) cache_write_tokens: u64,
}

/// One history event the driver replays. Text fields are `usize` indices into
/// the snapshot's shared text pool (`HistorySnapshot.texts`), so decoding a
/// repeated string costs one decode, not one decode per event.
#[derive(Clone)]
pub(crate) struct HistoryEvent {
    pub(crate) seq: u64,
    pub(crate) kind: HistoryEventKind,
}

#[derive(Clone)]
pub(crate) enum HistoryEventKind {
    TurnStart,
    TurnEnd {
        kind: TurnEndKind,
        error_message: Option<String>,
    },
    Message {
        turn: u64,
        step: u64,
        texts: Vec<String>,
        reasoning_texts: Vec<String>,
        usage: Option<TokenUsage>,
    },
    /// A step that streamed text/reasoning deltas but never produced an
    /// `assistant/message` (an aborted or interrupted turn). Its deltas are
    /// aggregated into one assembled text so history replay does not lose the
    /// partial reasoning/answer.
    StepDelta { reasoning: bool, text: String },
    ToolCall {
        call_id: String,
        name: String,
        /// Decoded `arguments` JSON string as produced by the model.
        arguments: Option<String>,
        view: Option<Value>,
    },
    ToolResult {
        call_id: Option<String>,
        content: Option<Value>,
        failed: bool,
        view: Option<Value>,
    },
    TodoWrite {
        todos: Value,
    },
    RequestContext {
        context_window: Option<u64>,
    },
    SessionTitle {
        title: Option<String>,
    },
}

// ---------------------------------------------------------------------------
// Page parsing and event conversion
// ---------------------------------------------------------------------------

/// Parse one raw HTTP history response body and validate the RPC envelope.
/// The returned page borrows `bytes`; it stays valid for as long as the byte
/// buffer does, which lets the caller collect raw text slices and convert to
/// owned events before dropping the buffer.
pub(crate) fn parse_history_response<'a>(
    bytes: &'a [u8],
    expected_rpc_id: &str,
) -> anyhow::Result<HistoryPage<'a>> {
    let envelope: Envelope = serde_json::from_slice(bytes)
        .context("DeepSeek Harness returned invalid session.history JSON")?;
    if envelope.kind != "server-response" || envelope.rpc_id != expected_rpc_id {
        return Err(anyhow!(
            "DeepSeek Harness returned an invalid session.history envelope"
        ));
    }
    if !envelope.result.ok {
        return Err(anyhow!(
            "DeepSeek Harness session.history failed (result.ok was false)"
        ));
    }
    let page: HistoryPage = serde_json::from_str(envelope.result.value.get())
        .context("DeepSeek Harness returned an invalid session.history value")?;
    Ok(page)
}

/// Collect every raw text slice the driver will read, in event order
/// (duplicates included). This is the input to the batch decode pass.
/// Convert one borrowed page into owned events. Text-bearing fields are
/// decoded directly from their raw slices — at message level there are only
/// a few hundred texts per page, so a deduplicated pool would save little.
pub(crate) fn convert_page<'a>(entries: &[HistoryEntry<'a>]) -> Vec<HistoryEvent> {
    entries.iter().filter_map(convert_entry).collect()
}

/// One text/reasoning delta of an orphaned step, addressed by its position
/// inside the page's raw bytes. The caller holds the page bytes alive until
/// the replay phase, so the delta payloads can be re-read (and aggregated
/// with a single decode) without keeping borrowed `&RawValue`s across the
/// fetch boundary.
pub(crate) struct DeltaRef {
    pub(crate) offset: usize,
    pub(crate) len: usize,
    pub(crate) reasoning: bool,
}

/// A step that streamed deltas but produced no `assistant/message` in its
/// own page: either truly aborted, or completed on a neighbour page (the
/// caller drops those after merging all pages).
pub(crate) struct OrphanCandidate {
    pub(crate) turn: u64,
    pub(crate) step: u64,
    pub(crate) reasoning_first_seq: u64,
    pub(crate) text_first_seq: u64,
    pub(crate) page: usize,
    pub(crate) deltas: Vec<DeltaRef>,
}

/// Scan one borrowed page and record delta references for steps that never
/// produced an `assistant/message` within it. Streaming, zero-copy: a step's
/// chunks arrive contiguously, so deltas are appended to the pending step and
/// sealed at its message (discarded — the deltas are folded into it) or at
/// any other event that proves the step ended without one.
pub(crate) fn scan_orphan_candidates<'a>(
    entries: &[HistoryEntry<'a>],
    page: usize,
    page_base: usize,
    out: &mut Vec<OrphanCandidate>,
) {
    type Pending = (u64, u64, Vec<DeltaRef>, u64, u64);
    let mut pending: Option<Pending> = None;

    macro_rules! seal {
        ($out:expr, $pending:expr) => {
            if let Some((turn, step, deltas, reasoning_first_seq, text_first_seq)) =
                $pending.take()
            {
                $out.push(OrphanCandidate {
                    turn,
                    step,
                    reasoning_first_seq,
                    text_first_seq,
                    page,
                    deltas,
                });
            }
        };
    }

    for entry in entries {
        let Some(event) = &entry.event else {
            continue;
        };
        let data = &event.data;
        match event.kind {
            "assistant/chunk" => {
                let Some(chunk) = &data.chunk else {
                    continue;
                };
                let Some(text) = &chunk.text else {
                    continue;
                };
                if !matches!(chunk.kind, "text-delta" | "reasoning-delta") {
                    continue;
                }
                let turn = data.turn.unwrap_or(0);
                let step = data.step.unwrap_or(0);
                if pending
                    .as_ref()
                    .is_some_and(|(current_turn, current_step, ..)| {
                        (*current_turn, *current_step) != (turn, step)
                    })
                {
                    seal!(out, pending);
                }
                let pending = pending
                    .get_or_insert_with(|| (turn, step, Vec::new(), u64::MAX, u64::MAX));
                let offset = text.get().as_ptr() as usize - page_base;
                let reasoning = chunk.kind == "reasoning-delta";
                if reasoning && pending.3 == u64::MAX {
                    pending.3 = event.seq;
                }
                if !reasoning && pending.4 == u64::MAX {
                    pending.4 = event.seq;
                }
                pending.2.push(DeltaRef {
                    offset,
                    len: text.get().len(),
                    reasoning,
                });
            }
            "assistant/message" => {
                let turn = data.turn.unwrap_or(0);
                let step = data.step.unwrap_or(0);
                if pending
                    .as_ref()
                    .is_some_and(|(current_turn, current_step, ..)| {
                        (*current_turn, *current_step) == (turn, step)
                    })
                {
                    pending = None;
                } else {
                    seal!(out, pending);
                }
            }
            _ => seal!(out, pending),
        }
    }
    seal!(out, pending);
}

/// Replay-phase aggregation: for every orphaned step that truly never got an
/// `assistant/message` across the whole fetch, concatenate its delta payloads
/// (read back from the caller-held page bytes) into one JSON string literal
/// and decode it once, producing one assembled `StepDelta` per reasoning/text
/// stream. The caller holds `pages` alive until this point, which is what
/// makes the deferred aggregation possible.
pub(crate) fn aggregate_orphan_deltas(
    pages: &[Vec<u8>],
    candidates: &[OrphanCandidate],
    message_steps: &std::collections::HashSet<(u64, u64)>,
) -> Vec<HistoryEvent> {
    let mut events = Vec::new();
    for candidate in candidates {
        if message_steps.contains(&(candidate.turn, candidate.step)) {
            continue;
        }
        let bytes = &pages[candidate.page];
        let reasoning: Vec<&str> = candidate
            .deltas
            .iter()
            .filter(|delta| delta.reasoning)
            .map(|delta| {
                std::str::from_utf8(&bytes[delta.offset..delta.offset + delta.len])
                    .expect("delta payload is UTF-8")
            })
            .collect();
        let text: Vec<&str> = candidate
            .deltas
            .iter()
            .filter(|delta| !delta.reasoning)
            .map(|delta| {
                std::str::from_utf8(&bytes[delta.offset..delta.offset + delta.len])
                    .expect("delta payload is UTF-8")
            })
            .collect();
        if !reasoning.is_empty() && let Ok(aggregated) = aggregate_delta_slices(&reasoning) {
            events.push(HistoryEvent {
                seq: candidate.reasoning_first_seq,
                kind: HistoryEventKind::StepDelta {
                    reasoning: true,
                    text: aggregated,
                },
            });
        }
        if !text.is_empty() && let Ok(aggregated) = aggregate_delta_slices(&text) {
            events.push(HistoryEvent {
                seq: candidate.text_first_seq,
                kind: HistoryEventKind::StepDelta {
                    reasoning: false,
                    text: aggregated,
                },
            });
        }
    }
    events
}

/// Concatenate escaped delta payloads (each a quoted, escaped JSON string)
/// into one JSON string literal and decode it once. Joining the interiors and
/// re-quoting yields the escaped concatenation of the decoded texts.
fn aggregate_delta_slices(raws: &[&str]) -> anyhow::Result<String> {
    let mut joined = String::with_capacity(
        raws.iter()
            .map(|slice| slice.len().saturating_sub(2))
            .sum::<usize>()
            .saturating_add(2),
    );
    joined.push('"');
    for slice in raws {
        joined.push_str(&slice[1..slice.len().saturating_sub(1)]);
    }
    joined.push('"');
    serde_json::from_str(&joined).context("aggregated delta text is not valid JSON")
}

fn convert_entry<'a>(entry: &HistoryEntry<'a>) -> Option<HistoryEvent> {
    let event = entry.event.as_ref()?;
    let data = &event.data;
    // Chunk events are dropped entirely (their deltas are folded into the
    // assembled assistant/message); do not even produce an Other event for
    // the ~45k chunk records per page.
    if event.kind == "assistant/chunk" {
        return None;
    }
    let kind = match event.kind {
        "turn/start" => HistoryEventKind::TurnStart,
        "turn/end" => {
            let (kind, error_message) = match &data.reason {
                Some(ReasonWire::Obj(reason)) => {
                    let kind = match reason.kind {
                        Some("completed") => TurnEndKind::Completed,
                        Some("aborted") => TurnEndKind::Aborted,
                        Some("blocked") => TurnEndKind::Blocked,
                        Some("error") => TurnEndKind::Error,
                        Some("max-tokens") => TurnEndKind::MaxTokens,
                        Some("interrupted") => TurnEndKind::Interrupted,
                        _ => TurnEndKind::Other,
                    };
                    let error_message = reason
                        .error
                        .as_ref()
                        .and_then(|error| error.message.as_ref())
                        .and_then(|message| decode_text(message));
                    (kind, error_message)
                }
                _ => (TurnEndKind::Other, None),
            };
            HistoryEventKind::TurnEnd {
                kind,
                error_message,
            }
        }
        // `assistant/chunk` events are dropped entirely: their token-level
        // deltas are already folded into the assembled `assistant/message`
        // (whose blocks carry the full text and usage), so history replay
        // never needs them.
        "assistant/message" => {
            let mut texts = Vec::new();
            let mut reasoning_texts = Vec::new();
            if let Some(blocks) = message_content_blocks(data.message) {
                for block in blocks {
                    let Some(text) = block.text.as_ref().and_then(|raw| decode_text(raw)) else {
                        continue;
                    };
                    match block.kind {
                        "text" => texts.push(text),
                        "reasoning" => reasoning_texts.push(text),
                        _ => {}
                    }
                }
            }
            let usage = data
                .usage
                .as_ref()
                .and_then(|usage| parse_usage(usage.get()).ok());
            HistoryEventKind::Message {
                turn: data.turn.unwrap_or(0),
                step: data.step.unwrap_or(0),
                texts,
                reasoning_texts,
                usage,
            }
        }
        "tool/call" => {
            let call_id = data.call_id?.to_owned();
            let name = data.name.unwrap_or("tool").to_owned();
            let arguments = data.arguments.as_ref().and_then(|raw| decode_text(raw));
            let view = entry
                .view
                .as_ref()
                .and_then(|view| serde_json::from_str::<Value>(view.get()).ok());
            HistoryEventKind::ToolCall {
                call_id,
                name,
                arguments,
                view,
            }
        }
        "tool/result" => {
            let message: Value = data
                .message
                .as_ref()
                .and_then(|message| serde_json::from_str::<Value>(message.get()).ok())
                .unwrap_or(Value::Null);
            let call_id = message
                .pointer("/source/callId")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .or_else(|| {
                    message
                        .pointer("/content/0/toolCallId")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                });
            let content = message.get("content").cloned();
            let failed = data.error.is_some()
                || message
                    .pointer("/content/0/isError")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
            let view = entry
                .view
                .as_ref()
                .and_then(|view| serde_json::from_str::<Value>(view.get()).ok());
            HistoryEventKind::ToolResult {
                call_id,
                content,
                failed,
                view,
            }
        }
        "todo/write" => {
            let todos = data
                .todos
                .as_ref()
                .and_then(|todos| serde_json::from_str::<Value>(todos.get()).ok())
                .unwrap_or_else(|| Value::Array(Vec::new()));
            HistoryEventKind::TodoWrite { todos }
        }
        "request/context" => HistoryEventKind::RequestContext {
            context_window: data.context_window,
        },
        "session/title" => {
            let title = data.title.as_ref().and_then(|raw| decode_text(raw));
            HistoryEventKind::SessionTitle { title }
        }
        // Unknown event types carry nothing the driver replays; skip them.
        _ => return None,
    };
    Some(HistoryEvent {
        seq: event.seq,
        kind,
    })
}

fn message_content_blocks<'a>(message: Option<&'a RawValue>) -> Option<Vec<ContentBlockWire<'a>>> {
    let message = message?;
    let parsed: MessageWire = serde_json::from_str(message.get()).ok()?;
    parsed.content
}

#[derive(Deserialize, Default)]
#[serde(bound(deserialize = "'de: 'a"))]
struct MessageWire<'a> {
    #[serde(default)]
    content: Option<Vec<ContentBlockWire<'a>>>,
}

#[derive(Deserialize, Default)]
#[serde(bound(deserialize = "'de: 'a"))]
struct ContentBlockWire<'a> {
    #[serde(rename = "type")]
    kind: &'a str,
    #[serde(default)]
    text: Option<&'a RawValue>,
}

/// Decode one escaped JSON string field into its text. A `RawValue` of a
/// string field is always valid JSON, so failure only guards against a
/// malformed page.
fn decode_text(raw: &RawValue) -> Option<String> {
    serde_json::from_str(raw.get()).ok()
}

fn parse_usage(raw: &str) -> anyhow::Result<TokenUsage> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct UsageWire {
        #[serde(default)]
        input_tokens: u64,
        #[serde(default)]
        output_tokens: u64,
        #[serde(default)]
        cache_read_tokens: u64,
        #[serde(default)]
        cache_write_tokens: u64,
    }
    let usage: UsageWire = serde_json::from_str(raw)?;
    Ok(TokenUsage {
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cache_read_tokens: usage.cache_read_tokens,
        cache_write_tokens: usage.cache_write_tokens,
    })
}

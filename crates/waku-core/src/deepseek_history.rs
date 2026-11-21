//! Typed, allocation-light reading of DeepSeek Harness `session.history` pages.
//!
//! One history response is an RPC envelope (`{type, rpcId, result: {ok,
//! value}}`) whose `value` is a page (`{events: [{event, view?}], hasMore,
//! projections?}`). Parsing that into `serde_json::Value` — the way the
//! driver used to read it — builds a value tree for every event, including
//! the token-level `assistant/chunk` deltas that carry a few bytes each. A
//! medium session is a few hundred thousand events, and the tree costs more
//! than the history itself.
//!
//! The page is read into typed structs instead. The sequence number and the
//! event kind are decoded; every other field stays a borrowed `&RawValue`
//! slice of the response bytes, so it is parsed only when a caller asks for
//! it. A page of deltas then costs no more than its sequence numbers.

use anyhow::{Context as _, anyhow, bail};
use serde::Deserialize;
use serde::Deserializer;
use serde::de::{IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde_json::Value;
use serde_json::value::RawValue;
use std::collections::HashSet;

/// The event kinds the driver reads. Every other kind is [`HistoryEventKind::Other`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum HistoryEventKind {
    TurnStart,
    TurnEnd,
    UserMessage,
    AssistantMessage,
    Other,
}

/// One event of a history page.
///
/// The payload is kept undecoded: a caller that only needs the sequence
/// numbers never pays for the text inside it.
pub(crate) struct HistoryEvent<'a> {
    /// The event's sequence number, when it carries one.
    pub(crate) seq: Option<u64>,
    pub(crate) kind: HistoryEventKind,
    /// The event's type as the harness wrote it, undecoded.
    pub(crate) kind_text: Option<&'a RawValue>,
    /// The event's own timestamp, undecoded.
    pub(crate) time: Option<&'a RawValue>,
    /// The event's payload, undecoded.
    pub(crate) data: Option<&'a RawValue>,
    /// The view the harness computed for the event, undecoded.
    pub(crate) view: Option<&'a RawValue>,
}

impl<'a> HistoryEvent<'a> {
    /// The event's payload, decoded. A payload that does not parse is read
    /// as absent, which is what the driver does with a field it cannot read.
    pub(crate) fn data_value(&self) -> Value {
        decode(self.data)
    }

    /// The event's timestamp, decoded.
    pub(crate) fn time_value(&self) -> Value {
        decode(self.time)
    }

    /// The view the harness computed for the event, decoded.
    pub(crate) fn view_value(&self) -> Option<Value> {
        self.view
            .map(|raw| decode(Some(raw)))
            .filter(|value| !value.is_null())
    }

    /// A copy of the event that owns its payload, for a caller that keeps
    /// events past the page they came from.
    pub(crate) fn to_owned_event(&self) -> OwnedHistoryEvent {
        OwnedHistoryEvent {
            seq: self.seq,
            kind: self.kind,
            time: self.time_value(),
            data: self.data_value(),
        }
    }
}

/// The event as the driver reads it: the fields it inspects, decoded.
///
/// A caller that replays history reads the event one at a time, so the
/// payload is decoded here rather than for every event on the page.
pub(crate) fn event_value(event: &HistoryEvent<'_>) -> Value {
    let mut value = serde_json::Map::new();
    if let Some(seq) = event.seq {
        value.insert("seq".to_owned(), Value::from(seq));
    }
    if let Some(kind) = event.kind_text.map(|raw| decode(Some(raw))) {
        value.insert("type".to_owned(), kind);
    }
    if let Some(data) = event.data {
        value.insert("data".to_owned(), decode(Some(data)));
    }
    Value::Object(value)
}

/// One event of a history page, with its payload decoded.
pub(crate) struct OwnedHistoryEvent {
    pub(crate) seq: Option<u64>,
    pub(crate) kind: HistoryEventKind,
    pub(crate) time: Value,
    pub(crate) data: Value,
}

/// One page of session history.
pub(crate) struct HistoryPage<'a> {
    pub(crate) events: Vec<HistoryEvent<'a>>,
    /// Whether the harness has older pages behind this one.
    pub(crate) has_more: bool,
    /// The session's projection values, undecoded.
    pub(crate) projections: Option<&'a RawValue>,
}

impl HistoryPage<'_> {
    /// The session's projection values, decoded.
    pub(crate) fn projection_values(&self) -> Option<Value> {
        let projections: WireProjections<'_> =
            serde_json::from_str(self.projections?.get()).ok()?;
        projections
            .values
            .map(|raw| decode(Some(raw)))
            .filter(|value| !value.is_null())
    }
}

/// Read one `session.history` response.
///
/// The response bytes are borrowed for as long as the page lives, so a page
/// must not outlive the bytes it was read from.
pub(crate) fn parse_history_response<'a>(
    bytes: &'a [u8],
    expected_rpc_id: &str,
) -> anyhow::Result<HistoryPage<'a>> {
    let envelope: WireEnvelope = serde_json::from_slice(bytes)
        .context("DeepSeek Harness returned invalid JSON for session.history")?;
    if !raw_text_is(envelope.kind.as_ref().copied(), "server-response")
        || !raw_text_is(envelope.rpc_id.as_ref().copied(), expected_rpc_id)
    {
        bail!("DeepSeek Harness returned an invalid session.history response");
    }
    let result = envelope
        .result
        .ok_or_else(|| anyhow!("DeepSeek Harness returned no session.history result"))?;
    if !result.ok {
        let error = result.error.unwrap_or(Value::Null);
        let code = error
            .get("code")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("unknown error");
        bail!("DeepSeek Harness session.history failed ({code}): {message}");
    }
    let page = result.value.unwrap_or_default();
    let events = page
        .events
        .0
        .into_iter()
        .filter_map(WireEntry::into_event)
        .collect();
    let has_more = page
        .has_more
        .and_then(|raw| serde_json::from_str(raw.get()).ok())
        .unwrap_or(false);
    Ok(HistoryPage {
        events,
        has_more,
        projections: page.projections,
    })
}

/// The lowest sequence number on the page, when it carries one.
pub(crate) fn oldest_seq(events: &[HistoryEvent<'_>]) -> Option<u64> {
    events.iter().filter_map(|event| event.seq).min()
}

/// Choose the next older-page cursor, stopping at sequence zero or a repeated cursor.
pub(crate) fn next_before_seq(
    has_more: bool,
    oldest: Option<u64>,
    seen: &mut HashSet<u64>,
) -> Option<u64> {
    if !has_more {
        return None;
    }
    let oldest = oldest.filter(|seq| *seq > 0)?;
    seen.insert(oldest).then_some(oldest)
}

// ── Wire shapes ─────────────────────────────────────────────────────────────

#[derive(Deserialize)]
#[serde(bound(deserialize = "'de: 'a"))]
struct WireEnvelope<'a> {
    #[serde(rename = "type", default)]
    kind: Option<&'a RawValue>,
    #[serde(rename = "rpcId", default)]
    rpc_id: Option<&'a RawValue>,
    #[serde(default)]
    result: Option<WireResult<'a>>,
}

#[derive(Deserialize)]
#[serde(bound(deserialize = "'de: 'a"))]
struct WireResult<'a> {
    #[serde(default)]
    ok: bool,
    #[serde(default)]
    value: Option<WirePage<'a>>,
    #[serde(default)]
    error: Option<Value>,
}

#[derive(Default, Deserialize)]
#[serde(bound(deserialize = "'de: 'a"))]
struct WirePage<'a> {
    #[serde(default)]
    events: WireEventList<'a>,
    #[serde(rename = "hasMore", default)]
    has_more: Option<&'a RawValue>,
    #[serde(default)]
    projections: Option<&'a RawValue>,
}

#[derive(Default)]
struct WireEventList<'a>(Vec<WireEntry<'a>>);

impl<'de: 'a, 'a> Deserialize<'de> for WireEventList<'a> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(WireEventListVisitor(std::marker::PhantomData))
    }
}

struct WireEventListVisitor<'a>(std::marker::PhantomData<&'a ()>);

impl<'de: 'a, 'a> Visitor<'de> for WireEventListVisitor<'a> {
    type Value = WireEventList<'a>;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("an array of history events")
    }

    fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut entries = Vec::new();
        while let Some(entry) = seq.next_element::<WireEntry<'a>>()? {
            entries.push(entry);
        }
        Ok(WireEventList(entries))
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        while map.next_key::<IgnoredAny>()?.is_some() {
            map.next_value::<IgnoredAny>()?;
        }
        Ok(WireEventList::default())
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(WireEventList::default())
    }

    fn visit_bool<E>(self, _: bool) -> Result<Self::Value, E> {
        Ok(WireEventList::default())
    }

    fn visit_i64<E>(self, _: i64) -> Result<Self::Value, E> {
        Ok(WireEventList::default())
    }

    fn visit_u64<E>(self, _: u64) -> Result<Self::Value, E> {
        Ok(WireEventList::default())
    }

    fn visit_f64<E>(self, _: f64) -> Result<Self::Value, E> {
        Ok(WireEventList::default())
    }

    fn visit_str<E>(self, _: &str) -> Result<Self::Value, E> {
        Ok(WireEventList::default())
    }

    fn visit_string<E>(self, _: String) -> Result<Self::Value, E> {
        Ok(WireEventList::default())
    }
}

#[derive(Deserialize)]
#[serde(bound(deserialize = "'de: 'a"))]
struct WireProjections<'a> {
    #[serde(default)]
    values: Option<&'a RawValue>,
}

/// One history item: `{event, view?}`. The host-computed tool `view` is kept
/// raw; it is parsed only for the events that present it. A flat item
/// carries the event's fields beside the view.
#[derive(Deserialize)]
#[serde(bound(deserialize = "'de: 'a"))]
struct WireEntry<'a> {
    #[serde(default)]
    event: Option<WireEvent<'a>>,
    /// A flat entry carries its sequence number beside the event.
    #[serde(default)]
    seq: Option<&'a RawValue>,
    #[serde(default)]
    view: Option<&'a RawValue>,
    #[serde(rename = "type", default)]
    kind: Option<&'a RawValue>,
    #[serde(default)]
    time: Option<&'a RawValue>,
    #[serde(default)]
    data: Option<&'a RawValue>,
}

/// One session event: `{type, seq, time, data, ...}`. Only the fields the
/// driver reads are captured, and they stay undecoded until it asks for them.
#[derive(Deserialize)]
#[serde(bound(deserialize = "'de: 'a"))]
struct WireEvent<'a> {
    #[serde(rename = "type", default)]
    kind: Option<&'a RawValue>,
    #[serde(default)]
    seq: Option<&'a RawValue>,
    #[serde(default)]
    time: Option<&'a RawValue>,
    #[serde(default)]
    data: Option<&'a RawValue>,
}

impl<'a> WireEntry<'a> {
    fn into_event(self) -> Option<HistoryEvent<'a>> {
        // An entry nests the event under `event`; a flat one carries the
        // event's fields beside the view.
        let kind_text = match self.event.as_ref().and_then(|event| event.kind) {
            Some(kind) => Some(kind),
            None => self.kind,
        };
        let seq = self.event.as_ref().and_then(|event| event.seq).or(self.seq);
        let time = match self.event.as_ref().and_then(|event| event.time) {
            Some(time) => Some(time),
            None => self.time,
        };
        let data = match self.event.as_ref().and_then(|event| event.data) {
            Some(data) => Some(data),
            None => self.data,
        };
        if kind_text.is_none() && seq.is_none() && data.is_none() {
            return None;
        }
        let kind = match kind_text.map(|kind| kind.get()) {
            Some("\"turn/start\"") => HistoryEventKind::TurnStart,
            Some("\"turn/end\"") => HistoryEventKind::TurnEnd,
            Some("\"user/message\"") => HistoryEventKind::UserMessage,
            Some("\"assistant/message\"") => HistoryEventKind::AssistantMessage,
            _ => HistoryEventKind::Other,
        };
        Some(HistoryEvent {
            seq: loose_u64(seq),
            kind,
            kind_text,
            time,
            data,
            view: self.view,
        })
    }
}

/// Decode a borrowed raw value. A value that does not parse reads as absent.
fn decode(raw: Option<&RawValue>) -> Value {
    raw.and_then(|raw| serde_json::from_str(raw.get()).ok())
        .unwrap_or(Value::Null)
}

/// Whether a raw value is the JSON string `expected`.
fn raw_text_is(raw: Option<&RawValue>, expected: &str) -> bool {
    raw.and_then(|raw| raw.get().strip_prefix('"')?.strip_suffix('"')) == Some(expected)
}

/// Read a sequence number that the harness sent as a number or as a string.
fn loose_u64(raw: Option<&RawValue>) -> Option<u64> {
    let raw = raw?;
    if let Ok(value) = raw.get().parse::<u64>() {
        return Some(value);
    }
    raw.get()
        .strip_prefix('"')
        .and_then(|text| text.strip_suffix('"'))
        .and_then(|text| text.parse::<u64>().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(json: &str) -> HistoryPage<'_> {
        parse_history_response(json.as_bytes(), "waku-1").expect("parse the history response")
    }

    /// One token-level event, the shape a session's history is mostly made of.
    fn delta_event(seq: u64) -> String {
        format!(
            r#"{{"event":{{"type":"assistant/chunk","seq":{seq},"data":{{"turn":1,"step":{seq},"chunk":{{"type":"text-delta","text":"tok"}}}}}}}}"#
        )
    }

    /// A `session.history` response carrying the given events.
    fn history_page(events: &[String]) -> String {
        format!(
            r#"{{"type":"server-response","rpcId":"waku-1","result":{{"ok":true,"value":{{"events":[{}],"hasMore":false}}}}}}"#,
            events.join(",")
        )
    }

    #[test]
    fn reads_the_sequence_numbers_and_kinds_of_a_page() {
        let page = page(
            r#"{"type":"server-response","rpcId":"waku-1","result":{"ok":true,"value":{
                "events":[
                    {"event":{"type":"turn/start","seq":1,"data":{}}},
                    {"event":{"type":"assistant/message","seq":2,"time":1700000000,
                              "data":{"message":{"content":[{"type":"text","text":"hi"}]}}}},
                    {"event":{"type":"turn/end","seq":3,"data":{"reason":{"kind":"completed"}}}}
                ],
                "hasMore":true,
                "projections":{"values":{"title":"a session"}}
            }}}"#,
        );

        assert!(page.has_more);
        assert_eq!(page.events.len(), 3);
        assert_eq!(page.events[0].kind, HistoryEventKind::TurnStart);
        assert_eq!(page.events[0].seq, Some(1));
        assert_eq!(page.events[1].kind, HistoryEventKind::AssistantMessage);
        assert_eq!(page.events[2].kind, HistoryEventKind::TurnEnd);
        assert_eq!(oldest_seq(&page.events), Some(1));
        assert_eq!(
            page.projection_values().expect("projection values"),
            serde_json::json!({"title": "a session"})
        );
        assert_eq!(
            page.events[1].data_value()["message"]["content"][0]["text"],
            serde_json::json!("hi")
        );
    }

    #[test]
    fn a_page_of_deltas_costs_only_its_sequence_numbers() {
        // The shape of a token-level page: every event carries a chunk of a
        // few bytes, and the driver reads none of it.
        let events = (1..=2_000).map(delta_event).collect::<Vec<_>>();
        let response = history_page(&events);

        let page = page(&response);

        assert_eq!(page.events.len(), 2_000);
        assert!(
            page.events
                .iter()
                .all(|event| event.kind == HistoryEventKind::Other)
        );
        assert_eq!(oldest_seq(&page.events), Some(1));
    }

    #[test]
    fn reads_a_sequence_number_sent_as_a_string() {
        let page = page(
            r#"{"type":"server-response","rpcId":"waku-1","result":{"ok":true,"value":{
                "events":[{"event":{"type":"turn/end","seq":"7","data":{}}}]
            }}}"#,
        );

        assert_eq!(page.events[0].seq, Some(7));
    }

    #[test]
    fn malformed_optional_page_fields_do_not_discard_valid_events() {
        let page = page(
            r#"{"type":"server-response","rpcId":"waku-1","result":{"ok":true,"value":{
                "events":[
                    {"event":{"type":"turn/start","seq":3,"data":{}}},
                    {"event":{"type":"turn/end","seq":4,"data":{}}}
                ],
                "hasMore":"not-a-boolean",
                "projections":[]
            }}}"#,
        );

        assert_eq!(page.events.len(), 2);
        assert_eq!(page.events[0].seq, Some(3));
        assert_eq!(page.events[1].seq, Some(4));
        assert!(!page.has_more);
        assert_eq!(page.projection_values(), None);
    }

    #[test]
    fn non_array_events_are_treated_as_empty() {
        let page = page(
            r#"{"type":"server-response","rpcId":"waku-1","result":{"ok":true,"value":{
                "events":{"unexpected":true},"hasMore":1,"projections":null
            }}}"#,
        );

        assert!(page.events.is_empty());
        assert!(!page.has_more);
        assert_eq!(page.projection_values(), None);
    }

    #[test]
    fn an_unknown_shape_does_not_fail_the_page() {
        // A page whose events carry fields this reader does not know, and an
        // event with no sequence number, still read.
        let page = page(
            r#"{"type":"server-response","rpcId":"waku-1","result":{"ok":true,"value":{
                "events":[
                    {"event":{"type":"tool/call","data":{"name":"read"}},"view":{"anything":true}},
                    {"event":{"type":"turn/end","seq":9,"data":{}}}
                ],
                "hasMore":false
            }}}"#,
        );

        assert_eq!(page.events.len(), 2);
        assert_eq!(page.events[0].kind, HistoryEventKind::Other);
        assert_eq!(page.events[0].seq, None);
        assert_eq!(page.events[1].seq, Some(9));
    }

    #[test]
    fn reads_an_event_that_is_not_nested() {
        // A flat item carries the event's fields beside the view.
        let page = page(
            r#"{"type":"server-response","rpcId":"waku-1","result":{"ok":true,"value":{
                "events":[{"type":"turn/end","seq":4,"data":{"reason":{"kind":"completed"}}}]
            }}}"#,
        );

        assert_eq!(page.events.len(), 1);
        assert_eq!(page.events[0].kind, HistoryEventKind::TurnEnd);
        assert_eq!(page.events[0].seq, Some(4));
    }

    #[test]
    fn history_pagination_stops_at_zero_and_repeated_cursors() {
        let mut seen = HashSet::new();

        assert_eq!(next_before_seq(true, Some(0), &mut seen), None);
        assert_eq!(next_before_seq(false, Some(4), &mut seen), None);
        assert_eq!(next_before_seq(true, Some(4), &mut seen), Some(4));
        assert_eq!(next_before_seq(true, Some(4), &mut seen), None);
    }

    #[test]
    fn a_response_for_another_request_is_rejected() {
        let error = parse_history_response(
            br#"{"type":"server-response","rpcId":"waku-other","result":{"ok":true,"value":{}}}"#,
            "waku-1",
        )
        .err()
        .expect("a response for another request");

        assert!(
            error
                .to_string()
                .contains("invalid session.history response"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn a_failed_response_carries_the_harness_error() {
        let error = parse_history_response(
            br#"{"type":"server-response","rpcId":"waku-1","result":{"ok":false,
                "error":{"code":"unknown_session","message":"no such session"}}}"#,
            "waku-1",
        )
        .err()
        .expect("a failed response");

        assert!(
            error
                .to_string()
                .contains("session.history failed (unknown_session): no such session"),
            "unexpected error: {error}"
        );
    }
}

/// The measurement behind the change: the same page read the way the driver
/// used to read it, and the way it reads it now.
///
/// `cargo test --locked -p waku-core --lib deepseek_history::bench -- --ignored --nocapture`
#[cfg(test)]
mod bench {
    use super::*;
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;
    use std::time::Instant;

    thread_local! {
        static COUNTING: Cell<bool> = const { Cell::new(false) };
        static COUNT: Cell<usize> = const { Cell::new(0) };
    }

    struct Counting;

    unsafe impl GlobalAlloc for Counting {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            if COUNTING.try_with(|c| c.get()).unwrap_or(false) {
                COUNT.with(|c| c.set(c.get() + 1));
            }
            unsafe { System.alloc(layout) }
        }
        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            unsafe { System.dealloc(ptr, layout) }
        }
        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            if COUNTING.try_with(|c| c.get()).unwrap_or(false) {
                COUNT.with(|c| c.set(c.get() + 1));
            }
            unsafe { System.realloc(ptr, layout, new_size) }
        }
    }

    #[global_allocator]
    static ALLOC: Counting = Counting;

    fn measure<T>(label: &str, body: impl FnOnce() -> T) -> T {
        COUNT.with(|c| c.set(0));
        COUNTING.with(|c| c.set(true));
        let started = Instant::now();
        let out = body();
        let elapsed = started.elapsed();
        COUNTING.with(|c| c.set(false));
        let allocations = COUNT.with(|c| c.get());
        println!("{label}: {elapsed:?}, {allocations} allocations");
        out
    }

    fn page(events: usize) -> Vec<u8> {
        let mut json = String::with_capacity(events * 160);
        json.push_str(
            r#"{"type":"server-response","rpcId":"waku-1","result":{"ok":true,"value":{"events":["#,
        );
        for seq in 1..=events {
            if seq > 1 {
                json.push(',');
            }
            json.push_str(r#"{"event":{"type":"assistant/chunk","seq":"#);
            json.push_str(&seq.to_string());
            json.push_str(r#","time":1700000000,"data":{"turn":1,"step":"#);
            json.push_str(&seq.to_string());
            json.push_str(r#","chunk":{"type":"text-delta","text":"tok"}}}}"#);
        }
        json.push_str(r#"],"hasMore":false,"projections":{"values":{"title":"t"}}}}}"#);
        json.into_bytes()
    }

    #[test]
    #[ignore = "a measurement, not a check"]
    fn benchmark() {
        let bytes = page(27_000);
        println!("page: {} bytes", bytes.len());

        let value = measure("value tree", || {
            serde_json::from_slice::<Value>(&bytes).expect("parse")
        });
        println!(
            "  value events: {}",
            value["result"]["value"]["events"]
                .as_array()
                .map(|a| a.len())
                .unwrap_or(0)
        );
        drop(value);

        let typed = measure("typed page", || {
            parse_history_response(&bytes, "waku-1").expect("parse")
        });
        println!("  typed events: {}", typed.events.len());
    }
}

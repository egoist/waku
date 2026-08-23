//! Shared framing for Pi-compatible RPC transports.

use serde_json::Value;

/// Oh My Pi refuses to reassemble beyond this, so neither should Waku.
const MAX_REASSEMBLED_FRAME_BYTES: usize = 64 * 1024 * 1024;

/// Reassembles the `rpc_chunk` runs Oh My Pi emits for frames over its 1 MiB
/// stdout ceiling.
#[derive(Default)]
pub(crate) struct ChunkAssembly {
    active: Option<PendingChunks>,
}

struct PendingChunks {
    chunk_id: String,
    count: u64,
    next_index: u64,
    byte_length: usize,
    data: Vec<u8>,
}

impl ChunkAssembly {
    /// Returns the logical message to dispatch, or `None` while a chunked
    /// frame is still arriving.
    pub(crate) fn accept(&mut self, value: Value) -> Result<Option<Value>, String> {
        if value.get("type").and_then(Value::as_str) != Some("rpc_chunk") {
            // The run must be uninterrupted, so anything else invalidates a
            // partial frame rather than silently splicing around it.
            if self.active.take().is_some() {
                return Err("chunked frame was interrupted".to_owned());
            }
            return Ok(Some(value));
        }
        let (chunk_id, index, count, byte_length, data) = (|| {
            Some((
                value.get("chunkId").and_then(Value::as_str)?,
                value.get("index").and_then(Value::as_u64)?,
                value.get("count").and_then(Value::as_u64)?,
                value.get("byteLength").and_then(Value::as_u64)?,
                value.get("data").and_then(Value::as_str)?,
            ))
        })()
        .ok_or_else(|| "chunk frame was malformed".to_owned())?;
        let byte_length = usize::try_from(byte_length)
            .map_err(|_| "chunked frame exceeds the reassembly limit".to_owned())?;
        if count == 0 || index >= count {
            self.active = None;
            return Err("chunk frame was malformed".to_owned());
        }
        if byte_length > MAX_REASSEMBLED_FRAME_BYTES {
            self.active = None;
            return Err("chunked frame exceeds the reassembly limit".to_owned());
        }
        let decoded = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, data)
            .map_err(|error| format!("chunk payload was not valid base64: {error}"))?;

        let pending = match self.active.take() {
            Some(pending)
                if pending.chunk_id == chunk_id
                    && pending.count == count
                    && pending.byte_length == byte_length
                    && pending.next_index == index =>
            {
                pending
            }
            Some(_) => {
                return Err("chunked frame was interrupted".to_owned());
            }
            None if index == 0 => PendingChunks {
                chunk_id: chunk_id.to_owned(),
                count,
                next_index: 0,
                byte_length,
                data: Vec::with_capacity(byte_length),
            },
            None => return Err("chunked frame started mid-sequence".to_owned()),
        };
        let mut pending = pending;
        pending.data.extend_from_slice(&decoded);
        pending.next_index += 1;
        if pending.data.len() > pending.byte_length {
            return Err("chunked frame overran its declared length".to_owned());
        }
        if pending.next_index < pending.count {
            self.active = Some(pending);
            return Ok(None);
        }
        if pending.data.len() != pending.byte_length {
            return Err("chunked frame did not match its declared length".to_owned());
        }
        let text = String::from_utf8(pending.data)
            .map_err(|_| "chunked frame was not valid UTF-8".to_owned())?;
        serde_json::from_str(&text)
            .map(Some)
            .map_err(|error| format!("chunked frame was not valid JSON: {error}"))
    }
}

#[cfg(test)]
mod tests {
    use base64::Engine as _;
    use serde_json::json;

    use super::*;

    #[test]
    fn chunked_frames_reassemble_and_reject_broken_runs() {
        let encode = |bytes: &[u8]| base64::engine::general_purpose::STANDARD.encode(bytes);
        let payload = json!({"type": "response", "id": "waku-1", "success": true});
        let bytes = serde_json::to_vec(&payload).unwrap();
        let (first, second) = bytes.split_at(bytes.len() / 2);
        let chunk = |index: u64, data: &[u8]| {
            json!({
                "type": "rpc_chunk",
                "chunkId": "rpc-1",
                "index": index,
                "count": 2,
                "byteLength": bytes.len(),
                "data": encode(data),
            })
        };

        let mut assembly = ChunkAssembly::default();
        assert_eq!(assembly.accept(chunk(0, first)).unwrap(), None);
        assert_eq!(assembly.accept(chunk(1, second)).unwrap(), Some(payload));

        let mut assembly = ChunkAssembly::default();
        let plain = json!({"type": "agent_start"});
        assert_eq!(assembly.accept(plain.clone()).unwrap(), Some(plain.clone()));

        let mut assembly = ChunkAssembly::default();
        assert_eq!(assembly.accept(chunk(0, first)).unwrap(), None);
        assert!(assembly.accept(plain).is_err());
        assert!(assembly.active.is_none());

        let mut assembly = ChunkAssembly::default();
        assert!(assembly.accept(chunk(1, second)).is_err());
    }
}

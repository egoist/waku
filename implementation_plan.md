# Agent-preset picker: wrong source provider + disappears after the first turn

## Symptoms

1. With provider = **OpenCode**, the agent chip lists agents that are not
   OpenCode's, and picking one does nothing.
2. The agent chip only exists before the first message; once the conversation
   has started, the button/popover is gone.

## Root causes

### 1. Presets are read from the wrong probe

`src/app/composer.rs:1698-1704`

```rust
let presets = self
    .provider_probe(ProviderKind::DeepSeek)   // hardcoded
    .map(|probe| probe.agent_presets.clone())
    .unwrap_or_default();
```

The session filter above it accepts `DeepSeek | OpenCode`, but the preset list
always comes from DeepSeek. Consequences for an OpenCode session:

- DeepSeek's 4 hardcoded fallback presets (`standard`/`code`/`minimal`/`cordis`)
  are rendered, because `provider_probe()` seeds every probe with
  `fallback_agent_presets()` (`crates/waku-core/src/model.rs:17`) and
  `fallback_agent_presets()` is non-empty **only** for DeepSeek
  (`crates/waku-core/src/model_catalog.rs:97`).
- Selecting one is then rejected: `set_agent_preset` validates against the
  *session's own* provider probe (`src/app/sessions.rs:1109-1117`), so the id is
  not found and the click is a silent no-op.

OpenCode does populate real presets — `discover_catalog` →
`discover_opencode_catalog` → `discover_opencode_agent_presets`
(`crates/waku-core/src/model_catalog.rs:130, 371-395`) — they were just never
read.

Secondary: `ProviderKind::OpenCode2` is user-selectable
(`src/app/composer.rs:3709` iterates `ProviderKind::ALL`) and also discovers
presets (`crates/waku-core/src/model_catalog.rs:131`), but is missing from all
three agent-preset gates.

### 2. The chip hides as soon as the session has started

`src/app/composer.rs:1695`

```rust
if session.has_started() || session.is_busy() {
    return None;
}
```

mirrored by the guard in `set_agent_preset` (`src/app/sessions.rs:1122-1123`).
That rule is correct for DeepSeek — the preset is baked into `session.create`
and never re-applied on resume (`crates/waku-core/src/driver/deepseek.rs:130-156`)
— but it was copied to OpenCode, where it is not:

- OpenCode v1 sends the agent on **every prompt/steer/command body**
  (`crates/waku-core/src/driver/opencode.rs:66-68, 493, 521, 552, 577`), so a
  runtime restarted with a new preset applies it to the next turn.
- OpenCode 2 explicitly switches agent when resuming
  (`crates/waku-core/src/driver/opencode2.rs:471-481` → `switch_agent`).

Precedent for "changeable mid-conversation": `AgentSession::can_choose_model`
(`crates/waku-protocol/src/model.rs:1204`) allows a model change after the
conversation started as long as the session is not busy, and `choose_model`
calls `reset_session_runtime` for exactly this
(`src/app/sessions.rs:950-951`).

## OpenCode docs (checked)

v1 (`opencode serve`, opencode.ai/docs/server):

- `GET /agent` → `Agent[]`, keyed by **`name`**, with
  `mode: "subagent" | "primary" | "all"` and optional `hidden`
  (`packages/opencode/src/agent/agent.ts`) — matches
  `parse_opencode_agent_presets`. Default is `default_agent`, else first
  visible non-subagent, else `build`.
- `POST /session/:id/message`, `/prompt_async`, `/command` all take an optional
  **`agent`** in the body → the agent is a per-turn property, so a runtime
  restarted with a new preset applies it on the next prompt.
- `POST /session` create body is documented as `{ parentID?, title? }` —
  `agent` is not documented there. Harmless: v1's effective agent comes from
  the per-prompt body. No change proposed.

v2 (`/api/...`, opencode.ai/v2/docs/api):

- `GET /api/agent` → `Agent.Info[]` with `{ id, name, description?, mode, hidden }`
  (default id `build`) — matches `opencode2_session::agent_presets`.
- `POST /api/session/{sessionID}/agent` body `{ agent }` —
  "switch the agent used by subsequent provider turns". Mid-session switching
  is an official, supported route.
- `POST /api/session` accepts `agent`.

Both confirm the plan: OpenCode can change agent on a live session; DeepSeek
cannot (no such route — the preset is fixed at `session.create`).

## Plan

### `crates/waku-protocol/src/model.rs`

1. Add to `impl ProviderKind`, beside `supports_model_discovery`:

   ```rust
   pub fn supports_agent_presets(self) -> bool {
       matches!(self, Self::DeepSeek | Self::OpenCode | Self::OpenCode2)
   }

   /// Whether an already-started session can be given a different agent.
   /// DeepSeek bakes the composition into session creation.
   pub fn supports_live_agent_switch(self) -> bool {
       matches!(self, Self::OpenCode | Self::OpenCode2)
   }
   ```

2. Add to `impl AgentSession`, beside `can_choose_model`:

   ```rust
   pub fn can_choose_agent_preset(&self) -> bool {
       self.provider.supports_agent_presets()
           && !self.is_busy()
           && (!self.has_started() || self.provider.supports_live_agent_switch())
   }
   ```

   Single source of truth for both the chip's visibility and the apply guard.

### `src/app/composer.rs` (`render_agent_preset_control`)

- Presets from the session's own provider:
  `self.provider_probe(session.provider)`.
- Visibility: `session.can_choose_agent_preset()`.

### `src/app/sessions.rs` (`set_agent_preset`)

- Provider filter → `provider.supports_agent_presets()`.
- Apply guard → replace `!session.has_started() && !session.is_busy()` with
  `session.can_choose_agent_preset()`. The existing `reset_session_runtime`
  call already makes the new agent take effect on the next turn (OpenCode
  re-sends it per prompt; OpenCode 2 switches it on resume).

### `src/app/runtime.rs` (`agent_preset_for_session`)

- Replace the inline `matches!(...)` with `session.provider.supports_agent_presets()`.

### Tests

- `crates/waku-protocol/src/model.rs` tests: `supports_agent_presets` /
  `supports_live_agent_switch` per provider (alongside the existing
  `assert!(ProviderKind::OpenCode2.supports_model_discovery())`).
- `can_choose_agent_preset`: DeepSeek started → false; OpenCode started and
  idle → true; any provider busy → false; OpenCode not started → true.

## Out of scope (flagging, not doing)

`apps/web/src/components/composer.tsx:1490-1495` has the same two bugs,
hardcoded to DeepSeek and to pre-start. Web is a separate client; say the word
and I will mirror the fix there.

## Verification

- `cargo fmt --check` (`cargo fmt --package waku --package waku-protocol ... -- --check`)
- `cargo test -p waku-protocol` and `cargo test -p waku`
- `bun run protocol:check`
- Manual, in the dev-watcher app: new OpenCode session → chip lists OpenCode
  agents; pick one → chip relabels; send a message → chip still present →
  switch agent → next reply runs under the new agent.

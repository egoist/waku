# Provider integrations

How Waku talks to each coding agent: the process it launches, the wire protocol
it speaks, how long that process lives, and what has to be emulated because the
CLI does not offer it.

How each of them names a session — which are read from the provider, which are
polled off disk, and the one Waku generates itself — is in
[titles.md](titles.md).

Every provider is reached through the same driver abstraction in
[driver/mod.rs](../crates/waku-core/src/driver/mod.rs). There are seven
transport implementations behind eleven providers, and **every one of them holds a
session that spans the whole conversation**:

| Transport | File | Providers |
| --- | --- | --- |
| Codex app-server (JSON-RPC over stdio) | [driver/codex.rs](../crates/waku-core/src/driver/codex.rs) | Codex CLI |
| Agent Client Protocol (JSON-RPC over stdio) | [driver/acp.rs](../crates/waku-core/src/driver/acp.rs) | Cursor CLI, Fx, Grok Build, Kimi Code |
| OpenCode server (HTTP + server-sent events) | [driver/opencode.rs](../crates/waku-core/src/driver/opencode.rs) | OpenCode |
| Pi RPC mode (NDJSON request/response over stdio) | [driver/pi.rs](../crates/waku-core/src/driver/pi.rs) | Pi, Oh My Pi |
| Claude streaming-input session (NDJSON over stdio) | [driver/claude.rs](../crates/waku-core/src/driver/claude.rs) | Claude Code |
| Amp streaming-JSON session (NDJSON over stdio) | [driver/amp.rs](../crates/waku-core/src/driver/amp.rs) | Amp |
| Harness client API (typed HTTP + downlink streams) | [driver/deepseek.rs](../crates/waku-core/src/driver/deepseek.rs) | DeepSeek Harness |

DeepSeek Harness has no dedicated section below yet; its driver's module
comment is the current reference.

## The driver contract

`driver::start(provider, DriverStartOptions, Sender<DriverEvent>)` returns a
`DriverHandle`. The UI never touches a process: it sends commands through
`DriverControl` and receives `DriverEvent`s on a `crossbeam` channel that the
frame loop drains.

Inputs ([driver/mod.rs:67](../crates/waku-core/src/driver/mod.rs#L79)):

```rust
pub struct DriverStartOptions {
    binary, cwd, mode, interaction_mode,
    model, reasoning_effort, service_tier,
    computer_use_enabled, provider_cursor,
}
```

Outputs ([model.rs:973](../crates/waku-core/src/model.rs)): `Connected`,
`AvailableCommands`, `TurnStarted`, `TextDelta`, `ReasoningDelta`, `Activity`,
`RichActivity`, `Permission`, `ComputerUseUpdated`, `SteerAccepted`,
`SteerRejected`, `TurnFinished`, `Error`, `ProcessExited`.

A transport that can inject a user message into the *running* turn advertises
it through `DriverControl::supports_steer` and delivers it with `steer`; the
outcome comes back asynchronously as `SteerAccepted` or `SteerRejected`. When
steering is unsupported, refused, or the session is still connecting, the app
falls back to its own follow-up queue — the message stays visible above the
composer and starts a fresh turn once the current one settles.

Every driver normalizes its tool events into one `ActivityItem`
(`Reasoning | Command | FileChange | Search | Plan | Tool`) via
[driver/activity.rs](../crates/waku-core/src/driver/activity.rs), so the transcript renders
provider-agnostic rows. Tool titles prefer a `title` argument when the tool
supplies one, then fall back to the command, the query, or a de-camel-cased
tool name.

### Runtime lifetime in the app

A driver is created lazily per session by `ensure_driver`
([src/app/runtime.rs:927](../src/app/runtime.rs#L1016)) and stored in
`Waku::runtimes` keyed by session id. Runtimes are per session, not per view:
switching sessions in the sidebar does not touch them, so a background session
keeps streaming into its transcript.

A runtime — and with it that session's provider process — is dropped when:

| Trigger | Where |
| --- | --- |
| The user stops a turn, **Codex and Amp only** | [src/app/sessions.rs:3](../src/app/sessions.rs#L3) |
| The provider changes, or an option changes that the transport cannot apply in session | `apply_session_options`, [src/app/runtime.rs](../src/app/runtime.rs) |
| The session is deleted | [src/app/sessions.rs:178](../src/app/sessions.rs#L178) |
| A rewind or branch leaves the driver on a stale native session | [src/app/runtime.rs](../src/app/runtime.rs) |
| The driver reports `ProcessExited` (the handler returns `false`, so the runtime is not reinserted) | [src/app/streaming.rs:352](../src/app/streaming.rs#L352) |
| Nobody has touched the session for 30 minutes | `reap_idle_sessions`, [src/app/runtime.rs](../src/app/runtime.rs) |
| Waku quits | `cx.quit()` |

Stop drops the runtime for Codex, whose app-server owns the Computer Use process
tree, and for Amp, which offers no interrupt on its stream — for both, stopping
means ending the process, and the next prompt resumes the native thread
(`thread/resume`, `threads continue`). Every other provider has a protocol
interrupt and keeps its runtime (`retain_runtime_after_cancel`).

Option changes go through `DriverControl::apply_options`, which returns whether
the transport absorbed the change or wants to be restarted:

| Change | Codex | Pi | ACP | OpenCode | Claude | Amp |
| --- | --- | --- | --- | --- | --- | --- |
| Model, reasoning effort, service tier | in session — they ride on every `turn/start` | in session — `set_model`, `set_thinking_level` | in session — `session/set_model`, except Fx's advertised `model` config option | in session — the model rides on each prompt | in session — a `set_model` control request | restart — all three are launch arguments |
| Access mode, interaction mode | restart | restart | restart | restart — the agent is chosen when the session opens | restart | restart |
| Provider | restart | restart | restart | restart | restart | restart |

The permission policy is deliberately excluded even for Codex, which does carry
`approvalPolicy` and `sandboxPolicy` on every `turn/start`: loosening or
tightening what an already-running agent may touch deserves a fresh thread. T3
Code draws the line in the same place — it restarts on `runtimeModeChanged` and
keeps the session only for a model change the adapter declares it can switch.

The idle sweep runs at most every 5 minutes off the existing frame tick and skips
any session with an active turn, so a slow tool call or an unanswered approval is
never reaped out from under the user.

Note what is *not* on the teardown list: finishing a turn. `TurnFinished` leaves
the long-lived processes resident and idle, which is the point of them — until
the idle sweep decides otherwise.

### How the long-lived processes actually die

Two shapes, depending on the transport.

**The stdio drivers — Codex, Pi, Claude, Amp, and the ACP agents — are never
signalled** (except when Stop ends Amp outright).
Termination is by **closing stdin**:

1. The driver is dropped, which sends `CommandMessage::Shutdown` (and drops the
   command `Sender`, so a missed send has the same effect).
2. The writer thread breaks out of its loop and returns, dropping the
   `ChildStdin` it owns.
3. The provider sees EOF on stdin and exits.
4. Its stdout closes, ending the reader thread, and `ProcessExited` is emitted.

So the process is asked to leave by having its input closed, and a provider that
ignored stdin EOF would linger. On quit the same thing happens for free:
`cx.quit()` may not run `Drop`, but the OS closes the descriptors, which is the
identical signal.

Each of these drivers moves its `Child` into a dedicated thread that blocks on
`wait()`, so the process is reaped and a non-zero exit status becomes an `Error`
when stderr has not already explained itself. Rust's `Child::drop` neither kills
nor reaps, so a driver that skipped that thread would leave a zombie for the life
of the app — which Pi did until it was given one.

**The OpenCode server is different**: it has no stdin to close, so
`OpenCodeServer`'s own `Drop` kills and waits on it
([opencode_session.rs](../crates/waku-core/src/opencode_session.rs)). Waku quitting without
running `Drop` is the one case that could orphan it, where the stdio drivers get
cleanup from the OS for free.

The other explicit kills are narrow and deliberate: Amp's process when the user
stops a turn, the short-lived servers that back a fork — OpenCode's and Grok's — and the
OpenCode server itself, whose driver kills it explicitly on drop.

## At a glance

| | Codex CLI | Pi | Oh My Pi | Claude Code | Amp | Cursor CLI | Fx | OpenCode | Grok Build | Kimi Code |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| Binary | `codex` | `pi` | `omp` | `claude` | `amp` | `cursor-agent` | `fx` | `opencode` | `grok` | `kimi` |
| Wire protocol | JSON-RPC over stdio | NDJSON RPC over stdio | NDJSON RPC over stdio | stream-json over stdio | stream-json over stdio | ACP over stdio | ACP over stdio | HTTP + SSE | ACP over stdio | ACP over stdio |
| Process spans the whole session | yes | yes | yes | yes | yes | yes | yes | yes | yes | yes |
| Process spawned per turn | no | no | no | no | no | no | no | no | no | no |
| Bidirectional | yes | yes | yes | yes | yes | yes | yes | yes | yes | yes |
| Reasoning stream | yes | yes | yes | yes | yes | yes | yes | yes | yes | yes |
| Interactive approvals | yes | no | no (has them; Waku runs `--yolo`) | yes | no | yes | yes | yes | yes | yes |
| Mid-turn steering | yes | yes | yes | yes | yes | yes | **no** | yes | yes | yes (transport) |
| Model discovery | yes | yes | yes | no (fixed) | no (modes) | yes | yes | yes | yes | yes |
| Computer Use | yes | yes | no (ships its own) | no | no | no | no | yes | yes | no |
| Restricted to Build + Full access | no | yes | yes | no | yes | no | no | no | no | no |
| Rewind and branch at a turn | yes | yes | yes | yes | yes | yes | **no** | yes | yes | **no** |

Kimi Code's steering is the transport's, not a probed policy: the ACP driver
sends the second `session/prompt` for every agent it drives, but Kimi's
superseded-prompt behaviour has not been observed against a live turn the way
Cursor's and Grok's were.

Every provider now holds a session across turns. That was not true when this
document was first written: five of the seven spawned a process per prompt, and
everything stateful — resume, rewind, branch, approvals — had to be reconstructed
from a session id, an on-disk transcript, or a side-channel. In each case the CLI
turned out to already serve a session protocol; nobody had looked.

---

## Codex CLI

**Launch** — `codex app-server --stdio`
([driver/codex.rs:164](../crates/waku-core/src/driver/codex.rs#L164)), plus `-c` config
overrides when Computer Use is on.

**Protocol** — newline-delimited JSON-RPC over stdio, genuinely bidirectional:
Codex can send Waku requests (approvals) and Waku answers them by id. Three
threads: writer (owns stdin and the command queue), reader (parses stdout),
stderr collector; a fourth waits on the process and emits `ProcessExited`.

**Lifetime** — long-lived: one app-server serves the whole session, staying
resident and idle between turns. It ends when the runtime is dropped — pressing
Stop, changing a launch option, deleting the session, or quitting — by closing
its stdin, never by a signal. See
[Runtime lifetime in the app](#runtime-lifetime-in-the-app).

**Handshake**

1. `initialize` (id `0`) with `clientInfo` and `capabilities.experimentalApi`.
2. `initialized`.
3. `skills/extraRoots/set` when Computer Use is on, so Waku's bundled skill is
   discovered like Codex's own skills rather than injected as instructions.
4. `thread/start` or `thread/resume` (id `1`) with `cwd`, `approvalPolicy`,
   `sandbox`, `approvalsReviewer`, and optional `model` / `serviceTier`.

The reply to id `1` carries `result.thread.id` (→ `Connected` with a
`ProviderResumeCursor::Codex`) and `result.thread.turns[]`, whose ids are
retained because `thread/fork` needs a `lastTurnId`.

**Per turn** — `turn/start` with `threadId`, `input: [{type: "text", …}]`,
`approvalPolicy`, `approvalsReviewer`, `sandboxPolicy`, and optional `model`,
`effort`, `serviceTier`.

**Inbound stream** ([driver/codex.rs:851](../crates/waku-core/src/driver/codex.rs#L882)):

| Method | Becomes |
| --- | --- |
| `turn/started` | `TurnStarted` (records the turn id) |
| `item/agentMessage/delta` | `TextDelta` |
| `item/reasoning/summaryTextDelta`, `item/reasoning/textDelta` | `ReasoningDelta` |
| `item/started`, `item/completed` | `RichActivity` (command, patch, web search, plan, MCP tool) |
| `turn/completed` | `TurnFinished { success: status == "completed" }` |
| `error`, `mcpServer/startupStatus/updated` (failed) | `Error` |
| `*requestApproval*` (a request, has an `id`) | `Permission` |

**Approvals** — Codex is the only provider with a real approval channel. The
request becomes a `Permission` event with `accept` / `acceptForSession` /
`decline`, and the answer is written back as a JSON-RPC *response*:
`{"id": <original>, "result": {"decision": …}}`. Because JSON-RPC ids are
per-peer, the reader only treats method-less messages as replies to Waku's own
requests ([driver/codex.rs:779](../crates/waku-core/src/driver/codex.rs#L809)).

**Cancel** — `turn/interrupt {threadId, turnId}`.

**Steer** — `turn/steer {threadId, expectedTurnId, input}`. The RPC response
resolves the pending steer to `SteerAccepted`, or to `SteerRejected` with the
CLI's reason when the expected turn no longer matches — the server-side check
that makes Codex the one provider whose steer cannot race a settling turn.

**Rewind** — `thread/rollback {threadId, numTurns}`, in place; the cursor is
unchanged. **Branch** — `thread/fork {threadId, lastTurnId}` returns a new
thread id. Both are synchronous from the UI's perspective: the command carries a
response channel and blocks up to 15 s.

**Citations** — Codex marks web citations with private-use characters
(`U+E200`/`U+E201`/`U+E202`). They are buffered across deltas and rewritten into
markdown links against the `webSearch` results captured earlier in the turn;
unknown markers are dropped. Private control markers never reach the transcript
([driver/codex.rs:660](../crates/waku-core/src/driver/codex.rs#L690)).

**Models** — a throwaway app-server, `model/list` paged via `nextCursor`, up to
32 pages ([model_catalog.rs:367](../crates/waku-core/src/model_catalog.rs#L367)).

**Computer Use** — `-c mcp_servers.waku_js_repl.command=…` registers Waku's
QuickJS MCP server, with several `-c` flags disabling Codex's own external
computer-use plugin/MCP/skill so only Waku's `js` / `js_reset` surface is
visible.

---

## Pi and Oh My Pi

Oh My Pi is a fork of Pi that kept the RPC transport and renamed part of its
surface, so one driver serves both. `PiFlavor`
([pi.rs:39](../crates/waku-core/src/driver/pi.rs#L39)) carries every divergence,
which is what keeps the two from drifting into near-copies:

| | Pi | Oh My Pi |
| --- | --- | --- |
| Binary | `pi` | `omp` |
| Full-access flag | `--approve` | `--yolo` |
| Update check | skipped by `PI_SKIP_VERSION_CHECK=1` | no env opt-out; gated by a setting, and off the startup path either way |
| Oversized frames | whole | chunked, once `negotiate_protocol {protocolVersion: 2}` is accepted |
| Run settles on | `agent_settled` | `agent_end` |
| Title event / field | `session_info_changed` / `name` | `session_info_update` / `title` |
| Branch commands | `get_fork_messages`, `fork` | `get_branch_messages`, `branch` |
| Whole-session copy | in place | only at launch, so Waku shells out (see below) |
| Computer Use | Waku's Pi extension | none — Oh My Pi ships its own `/computer` |
| Catalog probe's context-files flag | `--no-context-files` | `--no-rules` |

Everything below is shared unless noted.

**Launch** — `pi --mode rpc --approve` with `PI_SKIP_VERSION_CHECK=1`;
`omp --mode rpc --yolo`
([pi.rs:246](../crates/waku-core/src/driver/pi.rs#L246)). Oh My Pi negotiates
protocol v2 first, before `get_state`, so a large first response arrives chunked
rather than shrunk to an error frame. Its opening `ready` frame is what makes
that worth doing — it reports `supportedProtocolVersions: [1, 2]` alongside a
`maxFrameBytes` of 1 MiB and a `maxReassembledFrameBytes` of 64 MiB, so v1 caps
a response at the frame size while v2 reassembles up to 64× that.

Neither `--yolo` nor `--fork` appears in `omp --help`, but both are accepted
(verified against 17.3.8). Do not "fix" them by reading the help text: omp
rejects a genuinely unknown flag outright with `Error: unknown flag`, so the
absence is the help being abridged, not the flag being gone. That same
strictness is why its catalog probe cannot borrow Pi's argument list.

**Protocol** — NDJSON over stdio, but request/response rather than JSON-RPC:
Waku stamps each request with a string id (`waku-<n>`) and Pi answers with
`{"type": "response", "id", "success", "data"}`. Everything else on the stream
is an unsolicited event. Requests are issued synchronously by the writer thread
with a 10 s timeout ([pi.rs:800](../crates/waku-core/src/driver/pi.rs#L800));
events keep flowing on the reader thread meanwhile.

**Lifetime** — long-lived, and unlike Codex it survives Stop: cancelling sends
`abort` over the existing connection. It ends when the runtime is dropped, by
stdin EOF; nothing reaps it afterwards.

**Handshake** — `get_state` → optional `switch_session {sessionPath}` when
resuming → `set_model {provider, modelId}` → `set_thinking_level {level}` →
`get_state`. The final state supplies `/data/sessionId` and `/data/sessionFile`;
both go into the cursor, and resume needs the **file path**, not just the id.

**Per turn** — `{"type": "prompt", "message": …}`.

**Inbound stream** ([pi.rs:1182](../crates/waku-core/src/driver/pi.rs#L1182)):

| Event | Becomes |
| --- | --- |
| `agent_start`, `turn_start` | `TurnStarted` (once per run) |
| `message_update` → `text_delta` / `thinking_delta` | `TextDelta` / `ReasoningDelta` |
| `message_end` | fallback text/thinking when no delta was streamed |
| `tool_execution_start` / `_update` / `_end` | `RichActivity` |
| `auto_retry_end` | clears or sets the failure flag |
| `agent_settled` (Pi) / `agent_end` (Oh My Pi) | `TurnFinished`, then resets stream state |
| `extension_ui_request` | auto-cancelled — Waku has no UI for extension prompts |

**Access modes** — Build + Full access only, enforced at driver start rather
than degraded silently: any other combination fails with "currently supports
Build with Full access only" ([pi.rs:209](../crates/waku-core/src/driver/pi.rs#L209)).
Pi has no permission system at all, so `--approve` is the whole story. Oh My Pi
*does* have one, which Waku's `--yolo` then bypasses — the restriction is Waku's
here, not the CLI's, and lifting it is a matter of wiring Oh My Pi's permission
requests to a `Permission` event.

**Cancel** — `{"type": "abort"}`.

**Steer** — `{"type": "steer", "message": …}`; the request acknowledgment
resolves to `SteerAccepted` or `SteerRejected`.

**Rewind and branch** — both go through `get_fork_messages` → `fork {entryId}`
(`get_branch_messages` → `branch` on Oh My Pi), or `clone` when nothing is
removed, then `get_state`
([pi.rs:996](../crates/waku-core/src/driver/pi.rs#L996)). Rewind adopts the fork
as the session's new cursor. Branch additionally `switch_session`es back to the
source file and verifies it landed on the right session; if that restore fails
the runtime is dropped, because the RPC process may still be sitting on the fork
([runtime.rs](../src/app/runtime.rs)).

**Copying a whole session differs.** Removing no turns is a plain copy, which Pi
performs in place. Oh My Pi only copies at launch, so Waku shells out to a
throwaway `omp --mode rpc --yolo --fork <session file>` and reads the new cursor
off it ([pi.rs:1108](../crates/waku-core/src/driver/pi.rs#L1108)). That is the
better shape anyway: the out-of-process copy never moves the live session, so
unlike the in-place path it needs no restore afterwards and cannot strand the
RPC process on the fork.

**Models** — a separate `pi --mode rpc --no-session --no-skills
--no-prompt-templates --no-context-files` process answering
`get_available_models` and `get_state`. Extensions stay enabled because they can
register model providers. Ids are `provider/model` slugs and are validated as
such before launch.

Oh My Pi rejects unknown flags outright, so its probe is its own list —
`--no-session --no-skills --no-rules --no-extensions` — and the two describe
thinking differently. Pi maps levels through a per-model `thinkingLevelMap`; Oh
My Pi advertises the levels a model actually honors under `thinking.efforts`.
`off` never appears in that list because it bypasses provider mapping entirely,
yet it is always accepted, so it is added back
([model_catalog.rs](../crates/waku-core/src/model_catalog.rs)).

**Computer Use** — Pi only: `--extension <waku pi extension>` and
`--skill <SKILL.md>`, with the REPL and helper paths passed through the
environment. Waku's bridge is written against Pi's extension API, and Oh My Pi
ships its own `/computer` instead, so the flag is never passed to it.

---

## Claude Code

**Launch** — `claude -p --input-format stream-json --output-format stream-json
--verbose --include-partial-messages --replay-user-messages
--permission-prompt-tool stdio --permission-mode <mode>`
([driver/claude.rs](../crates/waku-core/src/driver/claude.rs)), plus `--model`, `--effort`,
and `--session-id` or `--resume`.

This is the transport the Claude Agent SDK's `query()` drives; the SDK is a
wrapper around these flags, not a separate capability, and there is no Rust SDK
to use instead. Both `--input-format stream-json` and `--permission-prompt-tool`
were verified against the real binary — **the latter is undocumented and absent
from `claude --help`**, and without it the CLI decides permissions itself and
only reports denials after the fact on `result`.

**Lifetime** — long-lived. One process serves the conversation, with turns fed
as newline-delimited user messages on stdin.

**Per turn** — write `{"type":"user","message":{"role":"user","content":[…]},
"parent_tool_use_id":null}`; the turn ends with a `result` message carrying
`is_error`, `stop_reason`, usage, and `permission_denials`.

**Inbound stream**

| Message | Becomes |
| --- | --- |
| `system` / `init` | the session id |
| `stream_event` → `text_delta`, `thinking_delta` | `TextDelta`, `ReasoningDelta` |
| `assistant` content blocks | `tool_use` → `RichActivity`; text and thinking only as a fallback when no delta of that kind streamed |
| `user` with `tool_result` | completes the matching activity |
| `user` with `isReplay: true` | ignored — Waku's own prompt echoed by `--replay-user-messages` |
| `result` | `TurnFinished` |
| `system` status/thinking-token notices, `rate_limit_event` | ignored |

**Approvals** — `control_request` / `subtype: "can_use_tool"` carries the tool
name, input, `tool_use_id`, the `blocked_path` that tripped the check, and
`permission_suggestions`. Waku answers with a `control_response` whose result is
`{"behavior":"allow"}` or `{"behavior":"deny","message":…}`. Outside Supervised it
answers allow itself.

**Cancel** — a `control_request` with `subtype: "interrupt"`.

**Steer** — the same user-message write as a prompt, sent while a turn is
running and without arming a new turn. The CLI holds the message and folds it
into the running turn at its next model call — one `result` still settles the
whole exchange, and the `isReplay` echo arrives at the moment of absorption
rather than at write time. Verified against the real CLI by injecting an
instruction while a Bash `sleep` ran: the same turn's reply honored it. Amp
was probed the same way and behaves differently — see its section.

**Model changes** — a `control_request` with `subtype: "set_model"`, so switching
models keeps the session. The permission posture is a launch flag and still
restarts.

**Native checkpoints** — after each turn Waku reads Claude's own transcript at
`$CLAUDE_CONFIG_DIR/projects/**/<session>.jsonl`, walks the `parentUuid` chain to
find the active branch, and records the latest message uuid as the turn's
`provider_resume_at` ([claude_session.rs](../crates/waku-core/src/claude_session.rs)). That
per-turn checkpoint is what makes rewind and branch possible. Because Claude
accepts a caller-chosen `--session-id`, the cursor exists before the first turn
does.

**Rewind and branch** — `claude_session::fork_session_at` rewrites the JSONL
transcript into a *new* session file, truncated at the checkpoint and re-keyed
with fresh uuids; the returned id map is applied to Waku's retained turns.
Rewinding to turn zero clears the cursor and starts clean. The CLI also exposes
`--fork-session` (with `--resume`), which likely replaces this hand-rolled
rewrite — unverified, and the reason it is still hand-rolled is that the flag was
found after the fork code was written.

**Models** — no discovery command; the catalog is a curated fixed list
([model_catalog.rs:52](../crates/waku-core/src/model_catalog.rs#L52)).

---

## Amp

**Launch** — `amp [threads continue <thread-id>] --execute --stream-json-thinking
--stream-json-input --dangerously-allow-all [--mode M] [--effort E] [--fast]`
([driver/amp.rs](../crates/waku-core/src/driver/amp.rs)). `--stream-json-thinking` implies
`--stream-json`, which `--stream-json-input` requires.

**Protocol** — newline-delimited JSON in both directions. Amp keeps the process
alive until *both* the assistant is done and stdin closes, which is what makes
one process serve the conversation.

**Lifetime** — long-lived. Turns are written as
`{"type":"user","message":{"role":"user","content":[…]}}`.

**Turn completion is not a `result` message.** Amp emits none; the turn is over
when an `assistant` message carries `stop_reason: "end_turn"`. A `tool_use` stop
reason is mid-turn. This was found by probing — a driver waiting for `result`
hangs forever.

**Inbound stream** — Anthropic-shaped: `system`/`init` carries the thread id;
`assistant` blocks carry text, thinking and `tool_use`; `user` blocks carry
`tool_result`. Redacted thinking is ignored rather than displayed. Text arrives
as whole blocks — Amp has no partial-message deltas.

**Access modes** — Build with Full access only; the driver refuses to start
otherwise. Amp's "models" are agent modes, and the fast service tier is `--fast`.
All three are launch arguments, so changing any of them restarts.

**Approvals** — none. Amp is the one long-lived provider that exposes no
permission request on its stream; its rules live in `amp permissions`, so Waku
still decides the posture at launch with `--dangerously-allow-all`.

**Cancel** — no stream interrupt exists, so Stop ends the process. The thread
survives on Amp's side and the next prompt resumes it with `threads continue`,
which is why Amp's runtime is not retained after a cancel.

**Steer** — the user message with a documented top-level `"steer": true`
attribute. A plain mid-turn message is held until the current turn's
`end_turn` and then runs as a turn of its own; the attribute marks it for
handling at the next interruption point instead, so the running turn absorbs
it and one `end_turn` settles everything. Both behaviors probed against the
real CLI — the plain-message probe is why an unmarked write must never be
used as a steer.

**Branch** — `amp threads export <id>` dumps the thread, Waku keeps the retained
prefix, `amp threads new` creates an empty thread, and the retained history is
replayed as a length-delimited envelope prepended to the first prompt
(`WAKU_AMP_BRANCH_CONTEXT_V1`). Forking a thread that was itself seeded this way
re-expands the nested envelope first, so branches of branches stay flat
([amp_session.rs](../crates/waku-core/src/amp_session.rs)).

---

## OpenCode server

**Launch** — `opencode serve --hostname 127.0.0.1 --port <ephemeral>`
([driver/opencode.rs](../crates/waku-core/src/driver/opencode.rs)). Waku already started this
server to fork a session; it now runs the conversation too.

**Protocol** — OpenCode's own HTTP API plus a server-sent event stream. Routes
and payloads here were read off a live server's OpenAPI document, not guessed.

**Lifetime** — long-lived: one server per session runtime.

**Handshake** — `POST /session` for a fresh session (or reuse the resume
cursor's id), then `POST /session/{id}/agent` to pick `plan` or `build`.

**Per turn** — `POST /session/{id}/prompt_async` with
`{parts: [{type: "text", …}]}`, which acknowledges with `204 No Content` as
soon as the prompt is accepted; the turn's completion arrives as
`session.idle` on the event stream. The blocking `message` route holds its
response until the turn ends — longer than any sane read timeout — so it is
not used for prompting. T3 Code's SDK calls the same route as
`session.promptAsync`.

**Steer** — the same `prompt_async` post while the session is busy: the
server folds the message into the running turn and one `session.idle` still
settles everything. OpenCode's own UI labels this "queued", but it is the
live turn absorbing the message, not a follow-up turn. The `204`
acknowledgment resolves to `SteerAccepted`; a failed post resolves to
`SteerRejected` and leaves the running turn untouched. Verified against a
real server by injecting an instruction while a bash `sleep` ran: one idle,
one reply, honoring both messages.

**Inbound stream** — `GET /event`, server-wide. The per-session route exists
only under `/api`, and since this server is Waku's alone, filtering by
`properties.sessionID` is enough — and necessary, so one task's traffic cannot
reach another's transcript.

| Event | Becomes |
| --- | --- |
| `message.part.delta`, `field: "text"` on a text or unknown part | `TextDelta` |
| `message.part.delta`, `field: "reasoning"` / `field: "thinking"`, or `field: "text"` on a native reasoning part | `ReasoningDelta` |
| `message.part.updated` with a `reasoning` / `thinking` part | records its `partID`, since OpenCode streams the part's content as the generic `text` field |
| `message.part.updated` with a `tool` part | `RichActivity`, read off `/state/status`, `/state/input`, `/state/output` |
| `message.updated` with assistant token counters | `UsageUpdated`, paired with `/api/model`'s context limit for the reported provider/model |
| `session.idle` | `TurnFinished` |
| `session.error` | `Error` |
| `permission.*` | `Permission` |
| `session.created`, `session.updated`, `session.diff`, plugin/catalog chatter | ignored |

**Approvals** — `POST /session/{id}/permission/{requestID}/reply` with
`{reply: "once" | "always" | "reject"}`. Supervised surfaces the request with the
permission's own patterns as the title; the auto modes answer `always` so the
agent stops asking about the same permission.

**Cancel** — `POST /session/{id}/abort`.

**Rewind and branch** — `POST /session/{id}/fork`. A live task sends the fork
through its resident server, avoiding a second OpenCode process contending for
the same local resources; a cold task may use a short-lived server
([opencode_session.rs](../crates/waku-core/src/opencode_session.rs)).

**Computer Use** — `OPENCODE_CONFIG_CONTENT` and the helper paths are handed to
the resident server through its environment, exactly as the one-shot invocation
received them.

---

## Agent Client Protocol

**Launch** — `cursor-agent acp`, `fx acp`, `grok agent [--reasoning-effort E] stdio`, `kimi acp`
([driver/acp.rs](../crates/waku-core/src/driver/acp.rs)).

**Protocol** — newline-delimited JSON-RPC over stdio, bidirectional. One agent
process serves the whole conversation, streams `session/update` notifications,
and asks the client for tool permission with a real request it expects an answer
to. Alongside Codex's app-server, this is the only transport where Waku's
Supervised mode means what it says.

**Lifetime** — long-lived, like Codex and Pi. Cursor and Grok previously spawned
a process per turn; Fx and Kimi Code arrived on this transport directly.

**Handshake** — `initialize` (advertising **no** `fs` or `terminal` client
capability, since Waku does not proxy the agent's file or terminal access — an
advertised capability the client cannot honor strands the agent mid-tool-call;
Cursor alone receives its `_meta.parameterizedModelPicker` opt-in) →
`session/resume` when resuming and the agent advertises it (so history is not
replayed), otherwise a replay-suppressed `session/load` when it reports
`loadSession`, else `session/new` → optional `session/set_mode`. A restore the
agent no longer recognizes falls back to a fresh session rather than stranding
the task. Mode selection is applied after both new and restored sessions. Kimi
Code advertises both, so it takes the first rung — `session/resume`, verified
against a session left by an earlier process.

Cursor's picker opt-in makes `session/new`, `session/load`, and
`session/resume` return provider-owned `configOptions`. Waku resolves the CLI's
flat model alias to the advertised `model` value, then applies any dynamic
`thought_level`, `thinking`, and `fast` options returned by that selection. If
an older Cursor agent advertises no model option, Waku retains the legacy
`session/set_model` request.

Fx also returns provider-owned config options, but its first model-category
option selects an account provider while the option whose id is `model` selects
the model. AI Gateway IDs such as `openai/gpt-5.6-luna-fast` are absent until
Waku first selects Fx's `gateway` provider option and reads the refreshed model
option from that response. Waku then targets the exact `model` id with
`session/set_config_option`; falling back to the older `session/set_model`
extension would not change Fx's model.

**Per turn** — `session/prompt`, whose response stays open until the turn ends.
It is tracked apart from the blocking request table precisely so the writer stays
free to send a cancel while it is outstanding; its reply is what emits
`TurnFinished`, keyed off `stopReason`.

**When `stopReason` lies.** Kimi Code answers a turn its model provider
rejected — an inactive plan, a spent quota — with a clean `end_turn` carrying no
content at all: no error, no JSON-RPC failure, nothing on stderr. Trusting the
protocol there shows the user an empty answer reported as a success, with no
cause to act on. The cause is recoverable, just not from the wire: Kimi appends
a `turn.ended` record with the real message to its own per-session log at
`<KIMI_CODE_HOME>/sessions/<workspace>/<session>/agents/main/wire.jsonl`.

[kimi_session.rs](../crates/waku-core/src/kimi_session.rs) reads it, and
`finish_prompt` lets a recovered failure override the protocol's verdict —
emitting `Error` with the provider's own wording and settling the turn
unsuccessfully. Three details make it safe:

- **It is scoped to a turn that produced nothing.** `AcpStreamState` tracks
  whether any message, thought, tool call, or plan arrived. A turn that streamed
  anything is settled by `stopReason` alone and does no I/O.
- **It waits.** The record lands *after* the ACP response — roughly 50ms in
  practice — so an immediate read finds nothing. The lookup polls, bounded at
  one second, and gives up quietly.
- **It ignores earlier turns.** The log's byte length is captured before the
  prompt is sent, and only what is appended past that offset is scanned, so a
  previous turn's failure can never be reported as this one's.

All of it runs on the driver thread, never a frame. The invariant it protects is
covered by `kimi_never_reports_an_empty_turn_as_a_success`, which passes whether
or not the account can currently serve a request.

**Inbound stream** — `session/update` notifications:

| `sessionUpdate` | Becomes |
| --- | --- |
| `agent_message_chunk` | `TextDelta` |
| `agent_thought_chunk` | `ReasoningDelta` |
| `tool_call`, `tool_call_update` | `RichActivity`, correlated by `toolCallId` |
| `plan` | a plan activity |
| `usage_update` | `UsageUpdated` — the context gauge, not transcript content |
| `available_commands_update` | `AvailableCommands` — the composer's slash-command list |
| `session_info_update` | `AutoTitleUpdated` when it carries a `title` |
| `user_message_chunk` | ignored — Waku's own prompt echoed back |

Everything outside `session/update` on that channel is agent-private control
traffic (Grok emits a stream of `_x.ai/*` notifications) and never reaches the
transcript.

Fx emits its context-limit and skill-discovery diagnostics as ordinary
`agent_message_chunk` updates before the model starts. Their reserved
`[context]` and `skill discovery warning:` prefixes are provider notices rather
than assistant content, so Waku filters that prelude from the transcript.

**Approvals** — `session/request_permission` becomes a `Permission` event whose
options come straight from the agent, with `kind` (`allow_once`, `allow_always`,
`reject_once`, `reject_always`) deciding which read as allow. The detail line is
the agent's own explanation from `toolCall.content` ("Not in allowlist: cat,
pwd") rather than a sentence synthesized from the tool kind — that reason is the
whole basis for the user's decision. Outside Supervised, Waku answers for the
user and prefers the durable allow so the agent stops asking about the same tool.

**Why the client advertises no `fs` or `terminal` capability.** Those declare
services *Waku offers the agent*, not permissions the agent needs. `fs` exists so
an editor can serve unsaved buffer contents in place of what is on disk, and
`terminal` lets the agent run commands through the client's own terminal. Waku
provides neither, so the agent uses its own read and shell tools and reaches the
filesystem exactly as before — verified against `cursor-agent acp` with both
declined: it read a file, ran a shell command, and ended the turn normally.
Advertising a capability Waku cannot service is the harmful choice, because the
agent would call `fs/read_text_file` and wait forever for a reply.

T3 Code lands in the same place: its `AcpSessionRuntime` defaults to
`fs.readTextFile: false`, `fs.writeTextFile: false`, `terminal: false`, Grok
passes no override, and Cursor's is only `_meta.parameterizedModelPicker`. The
handler registration points in its `packages/effect-acp` belong to a
general-purpose ACP library, not to the app that drives these two providers.

The one case that would justify serving `fs/read_text_file` is Waku's own file
editor, which tracks unsaved buffers
([src/app/right_panel.rs:1004](../src/app/right_panel.rs#L1004)): an agent
reading a file the user has unsaved edits in currently gets the disk copy. That
is a deliberate future call, not an oversight.

**Modes** — Plan maps to the agent's own `plan` mode via `session/set_mode` when
it advertises one; Cursor offers `agent`, `plan` and `ask`, Kimi Code offers
`default`, `plan`, `auto` and `yolo`. Fx offers only `ask` and `code`, so Waku
disables Plan for Fx, maps Supervised to `ask`, and maps the auto modes to
`code`. Every other access mode is Waku's to
enforce: the agent stays in the mode that asks, and `auto_approve` decides
whether Waku answers `session/request_permission` on the user's behalf. That is
why Kimi is left in `default` rather than being switched to `auto` or `yolo` —
the permission traffic is the feature, not an obstacle. Supervised deliberately
stays in `agent` mode: ACP's read-only `ask` mode *answers
questions* instead of asking permission, whereas Supervised means the agent still
acts, it just checks first — which is what `session/request_permission` already
does.

**Model and reasoning effort** — `session/set_model` after the session opens,
then the effort as a session config option. **The config id is the agent's to
name**, and the two disagree: Waku sends `mode` by default, but Kimi's `mode` is
its permission mode (`default`/`plan`/`auto`/`yolo`) and its effort lives on
`thinking`. `reasoning_effort_config_id` resolves that per provider — sending
the default id to Kimi would silently set nothing, or worse, move the permission
mode. The call is non-fatal either way, since an agent may expose no effort at
all. Grok is the exception: effort rides on `session/set_model` as
`_meta.reasoningEffort` (and as `--reasoning-effort` at launch), not as a
session config option.

Grok's catalog comes from the plain-text `grok models` listing, which reports
ids but no effort metadata. The hardcoded menu therefore covers only the exact
built-ins (`grok-4.5` stops at high, `grok-4.6` offers xhigh): the listing also
includes custom models from the user's config, whose effort support the id
alone cannot establish, so they are offered without an effort menu. Discovery
is authoritative — a stale fallback would name a model the CLI rejects.

Kimi's catalog comes from `kimi provider list --json`, which covers both the
managed plan and any registry the user imported with `kimi provider add`. Only
the K3 family reports `supportEfforts`; the rest expose a single always-on
thinking state, which is not a user choice and so is not offered as one. The
JSON omits the configured default, so the plain-text listing supplies that one
field — hence two probes
([model_catalog.rs](../crates/waku-core/src/model_catalog.rs)).

**Cancel** — `session/cancel`, a notification; the open `session/prompt` reports
the cancellation.

**Steer** — a second `session/prompt` while one is open. The agent continues
the same conversation under the newer request; the superseded request
resolves early — Cursor answers it `cancelled` the moment the steer lands and
re-plans with the message in context, Grok finishes the current work first
and answers the message before settling — and only the last open prompt's
response settles the merged turn. Both policies probed against the real
agents; T3 Code runs the same last-prompt-settles bookkeeping for both. Kimi
Code takes the same path by virtue of the transport, but its superseded-prompt
policy has not been probed against a live turn.

Fx allows only one active prompt per connection, so its driver does not
advertise steering. Follow-ups remain in Waku's queue and start after the
current prompt settles.

**Rewind and branch** — unchanged and still out of band: Grok forks through its
own ACP server plus on-disk truncation
([grok_session.rs](../crates/waku-core/src/grok_session.rs)), Cursor re-seeds a
fresh session ([cursor_session.rs](../crates/waku-core/src/cursor_session.rs)).

**Kimi Code and Fx have neither, deliberately.** Kimi advertises a `fork` session
capability, but `session/fork` takes only `{sessionId, cwd}` and copies the
whole conversation — there is no turn count, so "drop the last N turns" cannot
be expressed. Fx exposes no turn-aware fork or truncation method.
`ProviderKind::supports_conversation_fork` and
`supports_conversation_rollback` are therefore false for both, which hides the
rewind and branch affordances rather than offering a control that would silently
keep history the user asked to discard. The daemon and desktop match arms for it
exist only to keep the matches exhaustive; reaching them means the UI gate was
bypassed. Restoring these depends on Kimi accepting a truncation point.

**Computer Use** — Grok's isolated `GROK_HOME` and `--rules` setup is transport
independent, so the ACP session reuses the same builder the headless driver used.

**What moving to ACP gained.** Grok's Supervised mode no longer means "deny"
(`--permission-mode dontAsk` existed because the one-shot stream had no response
channel), Cursor's no longer means `--force`, and **Cursor streams reasoning**,
which its `--print` transport did not emit at all.

---

## Access modes across providers

Waku's `InteractionMode` (Build / Plan) and `RuntimeMode` (Supervised /
Auto-accept edits / Auto / Full access) collapse into each CLI's own vocabulary.
Plan always wins over the access mode.

| Waku | Codex (`approvalPolicy` / `sandbox` / reviewer) | Claude `--permission-mode` | Cursor | Fx | OpenCode | Grok | Kimi Code |
| --- | --- | --- | --- | --- | --- | --- | --- |
| Plan | `never` / `read-only` / `user` | `plan` | `session/set_mode` → `plan` | unsupported; control disabled | `agent: plan` | `session/set_mode` → `plan` | `session/set_mode` → `plan` |
| Supervised | `untrusted` / `read-only` / `user` | `default` + `can_use_tool` reaches the user | `session/request_permission` reaches the user | `session/set_mode` → `ask` | permission requests reach the user | `session/request_permission` reaches the user | `session/request_permission` reaches the user |
| Auto-accept edits | `on-request` / `workspace-write` / `user` | `acceptEdits` | auto-answered | `session/set_mode` → `code` | auto-answered (`always`) | auto-answered | auto-answered |
| Auto | `on-request` / `workspace-write` / `auto_review` | `auto` | auto-answered | `session/set_mode` → `code` | auto-answered (`always`) | auto-answered | auto-answered |
| Full access | `never` / `danger-full-access` / `user` | `bypassPermissions` + `--dangerously-skip-permissions` | auto-answered | `session/set_mode` → `code` | auto-answered (`always`) | auto-answered | auto-answered |

Amp, Pi, and Oh My Pi accept Build + Full access only and always run wide open
(`--dangerously-allow-all`, `--approve`, `--yolo`).

Every provider except those three distinguishes Supervised from the auto modes
in a way the user can actually answer. They decide by launch flag, so
"Supervised" degrades there to whatever the CLI does without a human at the
terminal — for Amp because its stream carries no permission request, for Pi
because it has no permission system to ask with, and for Oh My Pi because
`--yolo` bypasses the one it has. Only the last of those is Waku's own
limitation rather than the CLI's.

## Resume cursors

`ProviderResumeCursor` ([model.rs](../crates/waku-protocol/src/model.rs)) is
persisted with the session and is what makes a Waku task outlive its process:

| Provider | Cursor fields | Why |
| --- | --- | --- |
| Codex | `thread_id` | `thread/resume` |
| Pi | `session_id`, `session_file` | `switch_session` needs the path |
| Oh My Pi | `session_id`, `session_file` | same, plus `--fork <file>` for a whole-session copy |
| Claude | `session_id`, `resume_at` | `resume_at` is the transcript message uuid used for forking |
| Amp | `thread_id`, `fork_context` | `fork_context` is the seeded history for a branch |
| Cursor | `session_id`, `fork_context` | id is empty until a seeded branch streams one |
| Fx | `session_id` | `session/resume`; no fork or rewind, see above |
| OpenCode | `session_id` | `--session` / server fork |
| Grok | `session_id` | `--resume` / ACP fork |
| Kimi Code | `session_id` | `session/resume`; no fork, see above |

A cursor from the wrong provider is rejected at driver start rather than
silently ignored.

## Compared with T3 Code

[T3 Code](https://github.com/pingdotgg/t3code) solves the same problem with five
drivers — `codex`, `claudeAgent`, `cursor`, `grok`, `opencode` (no Amp, no Pi) —
registered in `apps/server/src/provider/builtInDrivers.ts` and documented in its
own `docs/internals/providers.md`.

**Its one structural difference: no provider is a per-turn process.** All five
hold a long-lived session; the transport differs, the lifetime does not.

| Provider | T3 Code transport | Waku transport |
| --- | --- | --- |
| Codex | `codex app-server` JSON-RPC (`packages/effect-codex-app-server`) | same |
| Claude | `@anthropic-ai/claude-agent-sdk` `query()` with an `AsyncIterable` prompt queue | same protocol, spoken directly — the SDK is a wrapper around these flags |
| Cursor | **`cursor-agent acp`** — ACP over stdio (`packages/effect-acp`) | same |
| Grok | **`grok agent stdio`** — ACP over stdio | same |
| OpenCode | long-lived `opencode serve` + HTTP SDK | same |

**All five now match**, and Claude reaches the same place without the SDK: there
is no Rust Agent SDK, but the SDK is a wrapper around the `claude` CLI's own
streaming-input protocol, which Waku speaks directly. No Node sidecar and no npm
dependency.

Waku goes one further than the comparison: Amp and Pi, which T3 Code does not
support, are long-lived here too. Every provider holds a session.

What the long-lived session buys, and what Waku pays for not having it:

| Capability | T3 Code | Waku |
| --- | --- | --- |
| Interactive approvals | Every provider: Claude via the SDK's `canUseTool` (including `AskUserQuestion` and `ExitPlanMode`), Cursor/Grok via ACP `session/request_permission`, Codex via `*requestApproval*` | Every provider except Amp and Pi, neither of which exposes a request to answer |
| Interrupt | `session/cancel`, `query.interrupt()` (plus `stopTask()` for runaway subagents) | Protocol interrupt everywhere except Amp, which has none and is stopped outright |
| Change model mid-session | `capabilities.sessionModelSwitch: "in-session"` → `session/set_model`, `query.setModel()` | Every transport keeps the session except Amp, whose mode is a launch argument |
| Mid-turn prompt | Queued into the live agent loop as a **steer**, same turn | Steered into the live turn on every provider (`⌘↩`); plain `Enter` queues a visible, editable follow-up instead |
| Native rollback | `rollbackThread` on the adapter contract | Codex/Pi natively; the rest emulated out-of-band by the `*_session.rs` helpers |
| Idle cleanup | `ProviderSessionReaper` stops sessions idle 30 min, swept every 5 min, skipping threads with an active turn | same, on the same thresholds |

The adapter contract itself is wider than `DriverControl`:
`startSession` / `sendTurn` / `interruptTurn` / `respondToRequest` /
`respondToUserInput` / `stopSession` / `listSessions` / `hasSession` /
`readThread` / `rollbackThread` / `stopAll` / `streamEvents`, plus a declared
`capabilities` record. Waku's equivalent surface is split between
`DriverControl` and the out-of-band `*_session.rs` helpers, which is why
capabilities like "can this provider fork?" live on `ProviderKind` rather than on
the driver that would have to implement them.

Note the parts that are *not* a gap. Waku's Codex path is the same app-server
protocol against the same methods. Both projects normalize provider events into
one canonical activity/event stream that the UI consumes provider-agnostically.
Both keep a per-session resume cursor and both had to special-case Claude's
transcript uuid as a rewind checkpoint.

## Adding a provider

1. Add the variant to `ProviderKind`
   ([model.rs](../crates/waku-protocol/src/model.rs)) with `id`,
   `display_name`, `short_name`, `command`, and the capability predicates. The
   compiler's non-exhaustive-match errors are the reliable to-do list for
   everything that follows.
2. Add a `ProviderResumeCursor` variant carrying whatever resume actually needs
   (an id is often not enough — see Pi's session file and Claude's message uuid).
3. Pick a transport, and look hard before settling for the one-shot path. Ask
   whether the CLI speaks ACP (`acp` / `agent stdio` — [driver/acp.rs](../crates/waku-core/src/driver/acp.rs)
   already covers it), serves an HTTP API, or has a persistent RPC mode; three
   providers were on `headless.rs` until someone checked. Only when none of those
   exist should you add a `parse_*` arm and an args builder to `headless.rs`.
   Route the choice in `driver::start`.
4. Map its stream onto `DriverEvent` and its tools onto `ActivityKind`. **Read
   the payloads off a live provider** — every driver here was written from a
   probe transcript or an OpenAPI document, and the two bugs that reached code
   anyway (a dead event subscription, a discarded permission reason) were both
   caught by running a real turn rather than by unit tests. Preserve ordering,
   and never leak private control markers into the transcript. If the transport
   accepts user messages mid-turn, probe *which* behavior it has before wiring
   `supports_steer`: inject an instruction while a slow tool runs and count the
   turn completions. Claude and OpenCode fold a plain message into the running
   turn; Amp queues it unless it carries the CLI's `"steer": true` attribute;
   ACP agents take a second `session/prompt` whose superseded predecessor must
   not settle the turn — and only a live probe tells these apart.
5. Map the access and interaction modes. If the transport can ask the user, route
   Supervised to a real `Permission` event; if it cannot, pick the safe
   degradation and say so in a comment at the call site.
6. Add an `#[ignore]`d integration test that drives the real provider through the
   driver, as `acp.rs` and `opencode.rs` do. It is the only check that catches a
   transport wired to nothing.
7. Implement rewind and branch, or emulate them the way Claude, Amp, Cursor,
   OpenCode and Grok do. Native truncation is preferable; seeding a fresh session
   with retained history is the fallback. If the provider offers neither — Kimi's
   fork takes no turn count — answer the capability predicates with false and let
   the UI hide the affordance. A control that silently keeps history the user
   asked to discard is worse than one that is not there.
8. **Do not trust a clean stop reason.** Probe what the provider does when the
   turn cannot run at all: an expired plan, a spent quota, a rejected key. Kimi
   reports `end_turn` with no content and no error, and the real message is only
   in its own session log — a client that believes the protocol shows an empty
   answer and calls it a success. Where the cause is recoverable, recover it;
   where it is not, at least do not report success for a turn that produced
   nothing.
9. Wire model discovery in `model_catalog.rs`, plus a fallback list for when the
   binary is missing or the command fails. Some transports hand you a better
   catalog than the CLI's `models` output — Cursor and Grok both return one in
   their ACP handshake.

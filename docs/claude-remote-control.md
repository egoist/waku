# Claude Remote Control from an Agent SDK host

This note documents a working way for an application that embeds Claude Code
through the Claude Agent SDK (or drives its `stream-json` protocol directly) to
make the same local session available in Claude Desktop, claude.ai, and the
Claude mobile app. It is intended as an implementation handoff for T3 Code,
Synara, and similar agent hosts.

The behavior below was verified on August 12, 2026 with Claude Code 2.1.228 and
`@anthropic-ai/claude-agent-sdk` 0.3.170. Remote Control is an official Claude
Code feature, but the SDK method used to enable it is currently undocumented.
Treat the exact method and event shapes as version-sensitive.

## The important part: do not pass `--remote-control` to `query()`

Passing `--remote-control` as an SDK `extraArgs` flag is a silent no-op. The
flag is handled by Claude Code's interactive terminal startup path, which is
not the path used by Agent SDK `query()` sessions.

Once the SDK query object exists, enable the bridge with its runtime method:

```ts
import { query } from "@anthropic-ai/claude-agent-sdk";

const session = query({
  prompt: promptStream,
  options: {
    // regular Agent SDK options
  },
});

type QueryWithRemoteControl = typeof session & {
  enableRemoteControl?: (enabled: boolean, name?: string) => Promise<unknown>;
};

await session.initializationResult();
await (session as QueryWithRemoteControl).enableRemoteControl?.(
  true,
  "Synara: task name",
);
```

The optional method is not in the public `Query` type, but in SDK 0.3.170 its
implementation sends this control request:

```json
{
  "type": "control_request",
  "request_id": "host-generated-id",
  "request": {
    "subtype": "remote_control",
    "enabled": true,
    "name": "Synara: task name"
  }
}
```

A host that drives the Claude Code executable directly does not need the SDK
dependency. Send the same JSON line on the existing `stream-json` stdin after
the process transport is ready and before the first user prompt. A caller-chosen
`--session-id <uuid>` continues to work and is useful for binding the provider
session to the host's thread.

Do not bypass Claude's policy result. The CLI checks account availability and
the organization's `allow_remote_control` policy itself. Surface a refusal or
error to the user rather than retrying around it.

## Connection state

Successful activation produces a system event whose subtype has appeared as
both `bridge_state` and `bridge_status` across builds. Handle both defensively.
The tested direct transport received:

```json
{
  "type": "system",
  "subtype": "bridge_state",
  "state": "ready"
}
```

Do not depend on a connection URL being present. Depending on the build, URL
fields have been observed or reported as `url`, `session_url`, `sessionUrl`, or
`bridgeUrl`, and a ready event may contain no URL at all. Once ready, the
session normally appears in the authenticated Claude clients without the host
opening a URL.

## Transcript synchronization is asymmetric

After activation, the normal SDK/`stream-json` output remains the canonical
source for Claude activity:

- assistant text and thinking;
- tool calls and tool results;
- permissions and other control requests;
- usage and the final result event.

This is why existing SDK rendering continues to work in the remote-controlled
turn, including thinking and tool activity.

There is one important exception in the tested Claude Code build: a human
prompt submitted from Claude Desktop was persisted to Claude's native JSONL
transcript but was not emitted as a user message on the host's `stream-json`
stdout. The assistant response *was* emitted on stdout.

Consequently, importing both sides of the remote turn from the JSONL file will
duplicate the assistant response. The working ownership split is:

| Content | Canonical source |
|---|---|
| Prompt submitted by the embedding host | Host's own submission state |
| Prompt submitted through Remote Control | Claude native JSONL transcript |
| Assistant text, thinking, and tools | SDK/`stream-json` stdout |
| Turn completion/result | SDK/`stream-json` stdout |

## Mirroring Remote Control prompts without duplicates

Claude stores a session under its configuration directory:

```text
$CLAUDE_CONFIG_DIR/projects/<encoded-cwd>/<session-id>.jsonl
```

When `CLAUDE_CONFIG_DIR` is unset, the default is:

```text
~/.claude/projects/<encoded-cwd>/<session-id>.jsonl
```

Recommended algorithm:

1. Start the Claude process with a known session UUID.
2. Locate `<session-id>.jsonl` below the configured `projects` directory. The
   file may not exist until Claude writes the first transcript entry.
3. If resuming an existing session, initialize the reader at EOF. Do not import
   its old history again.
4. Tail only newly appended, newline-terminated JSON records on a background
   thread/task. Never do filesystem I/O from a render path.
5. Import only a new external human `type: "user"` prompt.
6. Create the host-side turn before accepting the assistant deltas that follow
   on stdout.
7. Ignore assistant JSONL records. Continue consuming assistant output and the
   final result exclusively from the SDK/`stream-json` stream.

In the tested transcript, a Desktop prompt had this distinguishing shape:

```json
{
  "type": "user",
  "origin": { "kind": "human" },
  "message": {
    "role": "user",
    "content": "Prompt sent from Claude Desktop"
  }
}
```

A prompt submitted by the embedding host was persisted with no `origin` and
usually had `message.content` as an array of content blocks. This distinction
worked for Claude Code 2.1.228, but it is not a documented persistence contract.
Keep the detector narrow, test it against real provider payloads after CLI
updates, and never treat peer/channel/task-notification messages as human UI
input.

Also exclude:

- `isReplay: true` messages, which acknowledge host stdin when
  `--replay-user-messages` is enabled;
- `isSynthetic: true` messages;
- `shouldQuery: false` messages;
- user-role tool-result records;
- sidechain/subagent transcript entries.

A robust tailer should retain incomplete trailing bytes until the next read,
handle file truncation or replacement, stop with the provider process, and
deduplicate imported prompt UUIDs. Polling at roughly 100 ms is sufficient for
chat UI latency; a filesystem watcher is also suitable if it coalesces writes
and still parses only complete lines.

## Turn lifecycle

When the tailer observes a Remote Control prompt:

1. append the user message to the host transcript;
2. create or mark the host turn as running;
3. allow normal stdout assistant/thinking/tool events into that turn;
4. finish it only when the normal SDK result event arrives.

If an external prompt appears while the host already has an active turn, the
product must choose an explicit policy. Folding it into the active turn matches
Claude steering semantics; creating a competing foreground turn usually breaks
transcript ownership and completion bookkeeping.

## Operational and compatibility notes

- The local Claude process must stay alive for Remote Control to stay online.
- Extended network loss can terminate the bridge.
- Permissions still belong to the same local Claude process and configured
  permission mode.
- Remote Control availability depends on the user's plan and organization
  policy.
- Pin or test supported Claude Code versions because `enableRemoteControl`,
  bridge events, and the native transcript shape are not stable SDK contracts.
- Reading the user's own local transcript does not add network access or bypass
  authentication, but the host must protect that data like any other local chat
  history.

## Prior art

T3 Code explored this in
[PR #4666](https://github.com/pingdotgg/t3code/pull/4666). Its
[commit `2ed6a86`](https://github.com/pingdotgg/t3code/commit/2ed6a86b219dfd7248548eeda97b79bd9b26b569)
identified the SDK runtime method after confirming that the CLI flag under
`query()` was inert. The PR was closed because its orchestration-v1 base was
being replaced, not because Remote Control failed.

Anthropic documents Remote Control as an official Claude Code workflow and
supports enabling it for all sessions, but does not currently document the
Agent SDK method described above:

- [Claude Code power-user tips: Mobile and Remote Control](https://support.claude.com/en/articles/14554000-claude-code-power-user-tips)
- [Claude Code CLI reference](https://docs.anthropic.com/en/docs/claude-code/cli-usage)

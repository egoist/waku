# Pi and Oh My Pi extension dialogs

How Waku answers user questions raised by Pi extensions — the requirement, the
wire facts, the design, and where each piece lives. Companion to
[providers.md](providers.md); implementation in
[crates/waku-core/src/driver/pi.rs](../crates/waku-core/src/driver/pi.rs).

## Requirement

Pi extensions (and Oh My Pi's, which kept the same surface) ask the user
questions through the RPC frame `extension_ui_request`. An "ask the user" tool,
a confirmation prompt, a deploy-target picker — all of them arrive as one of
four dialog methods: `select`, `confirm`, `input`, `editor`.

Before this change Waku answered every such request with
`{"type": "extension_ui_response", "id": …, "cancelled": true}` the moment it
arrived — "auto-cancelled, Waku has no UI for extension prompts". The user
never saw the question; the extension only ever observed a cancellation, so
any workflow that needs a real answer was unusable on Pi sessions.

Meanwhile the shared structured-question pipeline
(`DriverEvent::UserInputRequested` → question panel → `Command::RespondUserInput`)
already existed for Claude (`AskUserQuestion`), Codex (`requestUserInput`),
OpenCode, DeepSeek, and the ACP providers. The requirement: route Pi's
extension dialogs through that same pipeline — no protocol, UI, or web-client
changes — with semantics that respect Pi's own dialog behavior.

## Wire facts

Verified against the installed `@earendil-works/pi-coding-agent` (Pi
`--mode rpc`) and `@oh-my-pi/pi-coding-agent`; both emit the same shapes:

| Method | Request fields (plus `id`) | Expected answer |
| --- | --- | --- |
| `select` | `title`, `options: string[]`, `timeout?` | `{id, value: <chosen label>}` |
| `confirm` | `title`, `message`, `timeout?` | `{id, confirmed: bool}` |
| `input` | `title`, `placeholder?`, `timeout?` | `{id, value: <typed text>}` |
| `editor` | `title`, `prefill?`, `promptStyle?` (Oh My Pi) | `{id, value: <edited text>}` |

Any dialog also accepts `{id, cancelled: true}`. Pi resolves a timed-out or
aborted dialog itself (with the caller's default), so a late answer is
harmless and unanswered dialogs never outlive their turn. `notify`,
`setStatus`, `setWidget`, `setTitle`, `set_editor_text` are fire-and-forget
and stay ignored.

Two more facts shape the design:

- **Preflight runs extension handlers.** A `prompt` request gets its
  response only after preflight succeeds, and `before_agent_start` handlers
  are part of it. A dialog raised there makes the prompt's ack exactly as
  slow as the user's answer.
- Questions are single-choice and text-only: `select` is not multi-select,
  options carry no descriptions, and `editor`'s `prefill` can be an entire
  file.

## Design

### Mapping onto the question model

Each dialog becomes exactly one `UserInputQuestion`; the original request id
doubles as the question id
([`extension_ui_request`](../crates/waku-core/src/driver/pi.rs#L246)):

| Method | header | question | options | answer kind |
| --- | --- | --- | --- | --- |
| `select` | `title` | `title` | one per label, trimmed, empties dropped | text of the picked label → `value` |
| `confirm` | `title` | `message` (falls back to `title`) | localized Yes/No | `true` iff the Yes label came back → `confirmed` |
| `input` | `title` | `placeholder` (falls back to `title`) | none — free text | typed text → `value` |
| `editor` | `title` | `title` | none — free text | edited text → `value` |

Lossy by intent: option descriptions, the input's separate placeholder field,
and the editor's `prefill` have no counterpart in the model and are not added
for one provider; placeholder text is good question copy, `prefill` is not.
An empty or blank answer is written as `cancelled: true`, matching how Pi
reads a non-answer
([`extension_ui_response_frame`](../crates/waku-core/src/driver/pi.rs#L345)).

The confirm case renders a boolean as two options. Pi only speaks the
boolean, so the queue entry remembers the rendered "yes" label
(`ExtensionAnswer::Confirmed { yes_label }`) and compares on the way back —
labels come from the new `user_input.yes` / `user_input.no` locale keys.

### Visibility semantics

Pi extensions may raise dialogs concurrently, but the UI holds one pending
question per session. `ExtensionRequestQueue`
([pi.rs:179](../crates/waku-core/src/driver/pi.rs#L179)) keeps requests in
arrival order and exposes one at a time:

- The first dialog emits `UserInputRequested` immediately; later ones wait.
- Answering removes the request and exposes the next queued one (emitted from
  the answering thread). Answers are matched by request id, and an id the
  queue no longer holds is a late answer: dropped silently.
- A terminal settle (`agent_settled` / terminal `agent_end`) forgets all
  unanswered dialogs without writing anything — Pi already resolved them when
  the turn aborted, and the UI drops its panel on `TurnFinished`.
- A dialog Waku cannot represent (e.g. `select` with no usable options) is
  still cancelled immediately, so its extension is never left hanging.
- Dropping the driver cancels every outstanding dialog before shutdown.

### Why stdin had to become shared

The first cut routed answers as `CommandMessage`s through the writer thread.
A real-RPC round trip exposed the deadlock: with a dialog raised by
`before_agent_start` still open, the writer thread is parked inside the
prompt's `send_request` (10 s timeout), the prompt's preflight is parked on
the dialog, and the answer is queued behind the prompt. Ten seconds later the
turn was declared failed even though the session was fine.

So stdin is now `SharedStdin = Arc<Mutex<Box<dyn Write + Send>>>`
([pi.rs:171](../crates/waku-core/src/driver/pi.rs#L171)), written from three
places, each holding the lock for exactly one NDJSON line:

- the writer thread: prompts, steers, setters, fork/rollback RPCs;
- the driver thread: `abort` on Stop, dialog answers (`respond_user_input`),
  and cancellation of leftovers on drop;
- the reader thread: immediate cancellation of unrepresentable dialogs.

Two consequences fell out of the same finding. The prompt's ack now waits
with **no deadline** ([`send_request_with_deadline`](../crates/waku-core/src/driver/pi.rs#L1040),
bounds everywhere else unchanged) — the turn's real outcome always arrives as
stream events, and process exit still breaks the wait via `fail_pending`.
And `CommandMessage` shed `Cancel`, `CancelExtensionRequest`, and
`RespondUserInput`, which the channel ordering had made unsafe.

## Testing

Unit tests around the queue, the mapping, and the response shapes live next
to the driver; `cargo test -p waku-core --lib driver::pi` covers:

- select/confirm/input mapping into `UserInputQuestion` (options, labels,
  yes/no booleans, free-text);
- empty and mismatched answers reading as `cancelled: true`;
- concurrent dialogs queueing and surfacing in order;
- settlement forgetting unanswered dialogs; unrepresentable dialogs
  cancelled; fire-and-forget methods ignored;
- answers written as exact NDJSON frames (asserted byte-for-byte against a
  captured stdin), with the next queued dialog exposed from the same call.

End-to-end, `extension_dialog_round_trips_against_the_real_rpc` is an
ignored test that installs a project extension under a temp cwd's
`.pi/extensions`, drives a real `pi` binary through a select dialog, answers
it, and asserts the turn settles. Run it with:

```sh
cargo test -p waku-core --lib driver::pi::tests::extension_dialog_round_trips -- --ignored
```

It needs an installed, authenticated Pi with network access — the preflight
finding above is exactly what this test caught.

## Limitations and follow-ups

- Sequential exposure only: dialogs queue one-deep against the single
  pending-question slot every provider shares; the second dialog appears when
  the first is answered, not side by side.
- `editor` drops `prefill`, and `input`'s placeholder doubles as the question
  text — clean but lossy, containing no protocol additions.
- Cross-turn dialogs cannot exist by Pi's semantics; the queue merely trusts
  that and self-heals on settle.
- Unrelated but adjacent: Oh My Pi's permission system remains bypassed via
  `--yolo`; wiring its permission requests to `Permission` events is a
  separate change ([providers.md](providers.md)).

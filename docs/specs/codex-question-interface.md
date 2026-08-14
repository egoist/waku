# Codex structured question interface

Status: Draft

## Summary

Add support for Codex app-server user-input requests so a Codex turn can ask the user one to three questions, collect the answers in Waku, and resume the same turn with the protocol response.

This is a Codex-only feature. It should use Codex's structured `item/tool/requestUserInput` request rather than extending the existing permission approval model.

## Goals

- Detect and parse Codex `item/tool/requestUserInput` JSON-RPC requests.
- Present all questions in the active session in a native, keyboard-operable inline card.
- Support option selection and Codex's `isOther` free-form answer path.
- Send the exact Codex response shape with the original JSON-RPC request ID.
- Keep the session in an appropriate waiting state while a blocking question is unanswered.
- Resume the existing Codex turn after the user submits answers.
- Clean up pending question state on response, turn completion, interruption, session switch, and driver shutdown.
- Keep Auto mode from answering user-input requests automatically.

## Non-goals

- Claude `AskUserQuestion` support.
- ACP, OpenCode, or DeepSeek question support.
- A provider-neutral form framework.
- Persistent transcript messages for questions and answers.
- MCP elicitation or other non-Codex form protocols.
- Automatic default answers, including in Auto mode or Auto Review mode.

## Existing behavior and constraints

Codex runs through `codex app-server --stdio` using JSON-RPC. Waku currently handles Codex `item` notifications and `requestApproval` requests, but the Codex parser ignores `item/tool/requestUserInput` because it only recognizes incoming requests whose method contains `requestApproval`.

The existing permission path is not a suitable representation for questions. Permission options are modeled as allow or deny decisions and are submitted immediately when a button is clicked. A question request needs multiple fields, per-question answer state, free-form input, and one final response containing all answers.

The implementation must preserve the current permission behavior unchanged.

## User experience

### Card placement and session state

Render a Codex question card in the same inline interaction area currently used for permission cards, above the composer. The card belongs to the active session only.

For a blocking request:

1. Receive the request and store it as the session's pending question.
2. Mark the session as waiting for user input.
3. Keep the composer available only if the existing app state allows it, but prevent a new turn from being submitted while the question is pending.
4. Submit answers only when the user activates the card's Submit action.
5. Send the JSON-RPC response and clear the card after the response is accepted by the driver.
6. Return the session to its normal working state as subsequent Codex events arrive.

For a non-blocking request, preserve the request's `isBlocking` value. The card may remain visible while the session continues working, but the response path is the same. If the current session state cannot safely represent a non-blocking interaction, treat it as blocking for the first implementation and record that behavior in a test.

Only one pending Codex question is required for the MVP. A second request received while one is pending must not overwrite the first. Report the condition through the existing driver error path and leave the first card intact.

### Question rendering

Render one card containing all questions, in the order received. Codex currently limits the request to one to three questions.

Each question displays:

- `header` as the short label.
- `question` as the primary prompt.
- Each option's `label` and `description` when options are present.
- An `Other` text control when `isOther` is true.

For the current Codex request shape, use one selected answer per question in the UI. The response still uses an array because that is the wire format and leaves room for future protocol changes.

If a question has no options, render a text input and require a non-empty answer. If `isOther` is true, selecting Other reveals a text input and the submitted answer must be the entered text. Do not send a placeholder or empty `user_note`.

The Submit action is disabled until every question has a valid answer. Selecting an option must not submit the request immediately. The user must be able to review and change all answers first.

The card must be keyboard-operable:

- Tab moves through options, text fields, and Submit.
- Arrow keys move within an option group where the existing control supports it.
- Enter or Space activates the focused option or Submit action.
- Focus has a visible treatment.
- Long labels and descriptions wrap without changing the answer semantics.

Do not rely on color, hover, or animation to communicate the selected state. The selected option needs a visible control state and text remains legible in both themes.

### Cancel and interruption

The MVP does not invent a Codex answer for cancellation. If the user stops the turn, switches away from the session, or the driver is interrupted, clear the pending card through the existing interruption and turn-finished paths. The normal Codex turn interruption flow is responsible for resolving the provider-side request.

If the product later adds a dedicated Cancel button, it should interrupt the Codex turn rather than submit a fabricated answer.

## Protocol contract

### Incoming request

Recognize the exact Codex method `item/tool/requestUserInput`. Keep the method match separate from the existing approval matcher. Do not classify it as a permission request.

The relevant request shape is:

```json
{
  "id": 42,
  "method": "item/tool/requestUserInput",
  "params": {
    "isBlocking": true,
    "questions": [
      {
        "id": "language",
        "header": "Language",
        "question": "Which language should I use?",
        "options": [
          { "label": "Rust", "description": "Use Rust" },
          { "label": "TypeScript", "description": "Use TypeScript" }
        ],
        "isOther": true
      }
    ]
  }
}
```

The parser must:

- Require one to three questions.
- Require a non-empty question ID and prompt.
- Preserve question order.
- Preserve option order, labels, and descriptions.
- Preserve `isOther` and `isBlocking`.
- Preserve the JSON-RPC request ID without losing whether it was numeric or string-valued.
- Tolerate unrelated future fields in `params`.

Malformed requests must fail safely. If an ID is available, send a JSON-RPC invalid-params error so Codex does not wait forever. Also emit the existing user-visible driver error event. Do not create a partial question card.

### Outgoing response

When the user submits, reply to the original request ID with this shape:

```json
{
  "id": 42,
  "result": {
    "answers": {
      "language": {
        "answers": ["Rust"]
      }
    }
  }
}
```

For an `Other` answer, send the entered text as the answer value. The Codex test client uses `user_note` for this path:

```json
{
  "id": 42,
  "result": {
    "answers": {
      "language": {
        "answers": ["user_note: the user's answer"]
      }
    }
  }
}
```

The response builder must be unit tested with both numeric and string request IDs, one question, and multiple questions. It must not reuse the permission response shape, which uses a `decision` field.

### Resolution notifications

Codex can emit a `serverRequest/resolved` notification. Handle it as internal lifecycle information. It must not appear in the transcript. If it refers to the pending question, clear the pending request unless the UI has already cleared it after submission.

## Data model and boundaries

Use a dedicated structured model rather than adapting `PermissionOption`:

```rust
struct UserInputQuestion {
    id: String,
    header: String,
    prompt: String,
    options: Vec<UserInputOption>,
    is_other: bool,
}

struct UserInputOption {
    label: String,
    description: Option<String>,
}

struct PendingCodexQuestion {
    request_id: RpcRequestId,
    is_blocking: bool,
    questions: Vec<UserInputQuestion>,
    answers: HashMap<String, Vec<String>>,
}
```

The exact type names may follow the repository's naming conventions. The important boundaries are:

- `DriverEvent::UserInputRequested` carries structured questions, not permission options.
- `SessionRuntime` owns the ephemeral pending question for the active turn.
- The UI owns draft selections and text input until Submit.
- The Codex driver owns JSON-RPC serialization and request ID handling.
- Pending question state is not persisted as transcript content.

Extend `DriverHandle` and `DriverControl` with a structured response method, for example `respond_user_input(request_id, answers)`. The default implementation for providers that do not support this capability should return an unsupported-operation error. Do not make the existing `respond(request_id, option_id)` accept arbitrary JSON or change its permission semantics.

If the existing string request ID helpers are reused, add round-trip tests proving that numeric and string IDs serialize correctly. Introducing a small internal `RpcRequestId` type is preferable if it avoids repeated string-to-JSON reconstruction.

## Implementation sequence

### 1. Codex transport and driver event

Update `src/driver/codex.rs` to:

- Match `item/tool/requestUserInput` before the generic approval path.
- Deserialize and validate the request.
- Emit `DriverEvent::UserInputRequested`.
- Add a command for structured answers.
- Serialize the response with the original request ID and the `answers` map.
- Handle `serverRequest/resolved` cleanup.
- Return a JSON-RPC error for malformed or unsupported requests.

Update `src/driver/mod.rs` and `src/model.rs` with the structured command, event, and data types.

No Codex initialize capability change is expected. Waku already enables the experimental app-server API used by the current Codex driver.

### 2. Session lifecycle

Update the runtime and streaming paths to:

- Store `PendingCodexQuestion` when the event arrives.
- Mark blocking requests as waiting.
- Keep the pending request scoped to the session.
- Clear it on response, `serverRequest/resolved`, turn completion, interruption, driver error, and shutdown.
- Include the new event in any force-save or event-pump matches without serializing the ephemeral draft.

The question path must not clear or mutate an unrelated pending permission.

### 3. Native question card

Add a dedicated renderer and response handler near the existing permission card implementation. The renderer should remain a small, purpose-built control for one to three questions rather than introducing a general form abstraction.

The response handler should validate all answers again before sending. On a driver error, keep the draft visible and show the existing error treatment so the user can retry or stop the turn.

### 4. Documentation

Update `docs/providers.md` to state that Codex supports structured `item/tool/requestUserInput` questions and that Waku keeps them separate from approval prompts. Document the current option and `Other` behavior.

## Auto mode behavior

Auto mode controls approval behavior. It is not a classifier and it must not infer answers to a user question.

When Codex sends `item/tool/requestUserInput`:

- Do not select the first option.
- Do not submit an empty answer.
- Do not auto-resolve it because the session is in Auto or Auto Review mode.
- Always surface the question card and wait for explicit user input.

This keeps approval automation and user intent collection as separate capabilities.

## Testing and verification

### Driver tests

- Parse a valid single-question request.
- Parse a valid three-question request and preserve order.
- Preserve option descriptions and `isOther`.
- Parse numeric and string JSON-RPC IDs.
- Reject zero questions, more than three questions, missing IDs, and missing prompts.
- Build the exact response for one and multiple questions.
- Build the `user_note` response for an Other answer.
- Verify question requests do not enter the permission response path.
- Verify malformed requests produce a JSON-RPC error and no pending UI event.

### Application tests

- A question event creates a pending question and blocking sessions enter Waiting.
- The Submit action remains disabled until every question is answered.
- Selecting and changing options updates draft state without sending a response.
- Other text input is required and is serialized correctly.
- Submission clears the card only after the driver response is queued successfully.
- Turn completion, interruption, session switching, and driver shutdown clear stale question state.
- Auto and Auto Review do not answer the request.
- Existing permission approval tests continue to pass unchanged.

### Live verification

Against the Codex version used by the development environment:

1. Start a Codex session that invokes `item/tool/requestUserInput`.
2. Verify the card appears in the active Waku session.
3. Answer an option-based question and confirm the same turn resumes.
4. Repeat with an Other answer and confirm the free-form value reaches Codex.
5. Stop the turn while the card is visible and confirm no stale card remains.
6. Repeat in Auto mode and confirm the request still waits for the user.

Run the repository formatter and the targeted Rust tests, followed by the normal project test and build checks. Validate the freshly rebuilt debug app for the exact Codex interaction because a successful Rust build does not verify the provider lifecycle.

## Acceptance criteria

- A valid Codex `item/tool/requestUserInput` request produces a visible question card in the correct session.
- One to three questions are rendered in protocol order.
- Option labels, descriptions, and the Other path are usable with mouse and keyboard.
- Submit sends the exact Codex `answers` response with the original request ID.
- The existing Codex turn resumes after a valid response.
- Auto mode never answers a question automatically.
- Permission approvals continue to use their existing `decision` response path.
- Pending question state cannot survive turn completion, interruption, session switching, or driver shutdown.
- Malformed requests fail without hanging Codex or leaving a partial card.
- Tests cover the wire format, lifecycle, validation, and Auto-mode boundary.

## References

- [Codex app-server README](https://github.com/openai/codex/blob/main/codex-rs/app-server/README.md)
- [Codex app-server user-input test client](https://github.com/openai/codex/blob/main/codex-rs/app-server-test-client/src/request_user_input.rs)

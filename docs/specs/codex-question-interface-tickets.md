# Tickets: Codex structured question interface

Status: Proposed

Source spec: [Codex structured question interface](./codex-question-interface.md)

## Goal

Allow a Codex app-server turn to ask one to three structured questions, collect answers in Waku, and resume the same turn with the correct JSON-RPC response.

Scope is limited to Codex. Claude, ACP, OpenCode, DeepSeek, MCP elicitation, and a provider-neutral form abstraction are excluded.

## Milestones

| Milestone | Tickets | Success criteria |
| --- | --- | --- |
| Transport contract | WAKU-CODEX-Q1, WAKU-CODEX-Q2 | Codex requests parse into structured events and answers serialize with the original request ID. |
| Session interaction | WAKU-CODEX-Q3, WAKU-CODEX-Q4, WAKU-CODEX-Q5 | A pending question is shown in the correct session, can be answered accessibly, and resumes the turn. |
| Verification and handoff | WAKU-CODEX-Q6, WAKU-CODEX-Q7 | Automated coverage, provider documentation, and live debug-app verification are complete. |

Estimated implementation effort: 36 hours, excluding review queue time and any Codex protocol change discovered during live verification.

## Tickets

### WAKU-CODEX-Q1: Add the structured user-input domain model

**Effort**: 4 hours  
**Owner**: TBD  
**Depends on**: None

Define the structured types needed for Codex questions and answers. Add a dedicated driver event and a structured response capability alongside the existing permission response path.

Likely areas:

- `src/model.rs`
- `src/driver/mod.rs`
- `src/driver/codex.rs` command plumbing, if required by the existing handle pattern

Done when:

- Questions contain IDs, headers, prompts, ordered options, descriptions, and `isOther`.
- Pending state can retain `isBlocking` and draft answers.
- `DriverEvent::UserInputRequested` is distinct from `DriverEvent::Permission`.
- `DriverHandle` and `DriverControl` can submit structured answers without changing permission decisions.
- Providers without this capability return an unsupported-operation error.
- No permission behavior or wire shape changes.

### WAKU-CODEX-Q2: Parse and respond to Codex user-input requests

**Effort**: 6 hours  
**Owner**: TBD  
**Depends on**: WAKU-CODEX-Q1

Teach the Codex driver to recognize `item/tool/requestUserInput`, validate it, emit the new event, and encode the response expected by Codex app-server.

Likely areas:

- `src/driver/codex.rs`
- Codex driver unit-test fixtures

Done when:

- The exact `item/tool/requestUserInput` method is matched separately from approval requests.
- One to three questions are accepted in protocol order.
- Missing IDs, prompts, or invalid question counts produce a JSON-RPC invalid-params error and no partial event.
- Option descriptions, `isOther`, and `isBlocking` are preserved.
- Numeric and string JSON-RPC request IDs round-trip without changing their JSON type.
- Responses use `result.answers`, keyed by question ID, with each answer represented as an array.
- `Other` answers use the Codex `user_note` convention.
- `serverRequest/resolved` is handled as internal lifecycle information and is not rendered in the transcript.

### WAKU-CODEX-Q3: Integrate pending questions into session lifecycle

**Effort**: 4 hours  
**Owner**: TBD  
**Depends on**: WAKU-CODEX-Q1, WAKU-CODEX-Q2

Connect the new driver event to session runtime state and turn lifecycle handling.

Likely areas:

- `src/app.rs`
- `src/app/streaming.rs`
- `src/app/runtime.rs`
- `src/app/sessions.rs`

Done when:

- The event creates pending question state on the receiving session only.
- Blocking requests put the session in its waiting state.
- A second pending question cannot overwrite the first.
- Pending state is cleared on successful response, `serverRequest/resolved`, turn completion, interruption, driver error, shutdown, and session switch.
- Pending questions remain ephemeral and are not serialized as transcript content.
- Pending permissions continue to work independently.
- Auto and Auto Review do not answer or dismiss the request.

### WAKU-CODEX-Q4: Build the native Codex question card

**Effort**: 8 hours  
**Owner**: TBD  
**Depends on**: WAKU-CODEX-Q1, WAKU-CODEX-Q3

Add the inline question UI above the composer, reusing the placement and visual language of the permission card while keeping question semantics separate.

Likely areas:

- `src/app/composer.rs`
- `src/app/render.rs`
- Existing UI localization file, if labels are localized there

Done when:

- One to three questions render in protocol order.
- Headers, prompts, option labels, and option descriptions are visible.
- Each question supports one selected option in the current Codex schema.
- Questions without options render a required text input.
- `isOther` renders an Other text path and requires non-empty text.
- Submit stays disabled until every question has a valid answer.
- Selections and text are editable before submission.
- Mouse and keyboard interaction work with visible focus and selection states.
- The UI remains legible in both themes and does not rely on color or hover alone.

### WAKU-CODEX-Q5: Connect submission, errors, and interruption behavior

**Effort**: 4 hours  
**Owner**: TBD  
**Depends on**: WAKU-CODEX-Q2, WAKU-CODEX-Q3, WAKU-CODEX-Q4

Complete the user action path from the question card to the Codex driver and make failures recoverable.

Likely areas:

- `src/app/sessions.rs`
- `src/app/composer.rs`
- `src/app/runtime.rs`

Done when:

- Submit validates every answer again before invoking the structured driver response.
- The original request ID and all question IDs are passed through unchanged.
- The card clears only after the response is queued successfully.
- A driver error leaves the draft visible and uses the existing error treatment.
- Stop, interruption, session switching, and driver shutdown do not leave a stale card.
- The normal permission response path remains unchanged.
- Auto mode never chooses a default option or submits an empty answer.

### WAKU-CODEX-Q6: Add transport, lifecycle, and regression coverage

**Effort**: 6 hours  
**Owner**: TBD  
**Depends on**: WAKU-CODEX-Q2, WAKU-CODEX-Q3, WAKU-CODEX-Q4, WAKU-CODEX-Q5

Add automated coverage for the wire contract and the app behavior that can be verified without a live Codex process.

Done when:

- Driver tests cover valid one-question and three-question requests.
- Tests cover numeric and string request IDs.
- Tests cover option answers, multiple question answers, and `user_note` answers.
- Tests cover malformed requests and confirm no partial pending state is created.
- App tests cover waiting state, incomplete submission, answer changes, response success, response failure, and cleanup paths.
- Tests confirm Auto and Auto Review leave questions for the user.
- Existing permission approval tests pass unchanged.
- Formatting, targeted Rust tests, and the normal project checks pass.

### WAKU-CODEX-Q7: Document and live-verify the provider interaction

**Effort**: 4 hours  
**Owner**: TBD  
**Depends on**: WAKU-CODEX-Q5, WAKU-CODEX-Q6

Document the supported Codex interaction and verify it against the Codex version used by the development environment.

Likely areas:

- `docs/providers.md`
- The freshly rebuilt debug app managed by the existing development watcher

Done when:

- Provider documentation describes `item/tool/requestUserInput` separately from approval requests.
- Documentation states the current one-option-per-question UI and Other behavior.
- A live Codex request displays in the correct Waku session.
- An option answer resumes the same Codex turn.
- An Other answer reaches Codex as the expected free-form value.
- Stopping the turn removes the pending card.
- Auto mode still waits for explicit user input.
- Any protocol mismatch is recorded as a follow-up ticket rather than silently worked around.

## Dependency map

```text
WAKU-CODEX-Q1
    ├──> WAKU-CODEX-Q2 ──┐
    └──> WAKU-CODEX-Q3 ──┼──> WAKU-CODEX-Q4 ──> WAKU-CODEX-Q5 ──> WAKU-CODEX-Q6 ──> WAKU-CODEX-Q7
                         └───────────────────────────────────────────────────────────────────────┘
```

Q2 and Q3 can proceed in parallel after Q1. Q4 depends on the lifecycle shape from Q3, while Q5 is the integration point for the transport and UI paths.

## Risks and follow-ups

| Risk | Impact | Probability | Mitigation |
| --- | --- | --- | --- |
| Codex app-server changes the request or response schema | High | Medium | Keep protocol fixtures, preserve unknown fields, and run the live verification ticket against the installed Codex version. |
| JSON-RPC request IDs are reconstructed incorrectly | High | Low | Test both numeric and string IDs, or use an internal ID type that preserves the original JSON value. |
| Question state becomes coupled to permission state | Medium | Medium | Keep a dedicated event, pending model, response method, and renderer. |
| A non-blocking request conflicts with Waku's single pending interaction model | Medium | Low | Preserve `isBlocking`, test the observed behavior, and create a follow-up if concurrent interactions are required. |
| UI focus or cleanup regresses existing composer behavior | Medium | Medium | Include keyboard and interruption cases in Q6 and validate the freshly rebuilt debug app in Q7. |

## Suggested merge order

Merge Q1 first, then Q2 and Q3 in parallel. Merge Q4 after Q3. Merge Q5 after Q2 through Q4. Finish with Q6 and Q7. No ticket should expand the scope to Claude, ACP, or a shared provider question abstraction.

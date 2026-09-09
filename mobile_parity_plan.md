# Mobile parity (branch `feat/mobile-parity`)

Base: `fix/local-daemon-socket-reconnect` @ `7bd92d1`, working tree carried over
uncommitted (agent-preset fix + pre-existing WIP + regenerated bindings).

Ground rules for this branch: **local only.** Nothing committed, no upstream set,
nothing pushed. Stop after any phase — each one is independently useful.

## Why

The daemon already syncs both clients: it owns task state + SQLite, merges
`SaveTaskState` per session with a staleness guard
(`crates/waku-core/src/daemon.rs:386-405`), and `broadcast_task_state_changed`
notifies every other subscriber (`crates/waku-core/src/server.rs:381`). So the
phone is a second screen on the same tasks — the gap is surface area, not data.

Mobile currently drives ~20 of the ~45 protocol commands. Below is the work,
ordered by what hurts when you are away from the desk.

## Phase 0 — Guardrails (0.5 d)

1. **CI.** `.github/workflows/test.yml` runs only `cargo test` and
   `@waku/client`. Add:
   ```yaml
   - run: bun --filter @waku/mobile typecheck
   - run: bun --filter @waku/mobile test
   ```
   Without it every protocol change (today's agent-preset work included) can
   drift past mobile silently. **Done** — added under "Check mobile client",
   gated to the Linux runner beside the existing JS steps.
2. **Build once and drive a real task** — see "Building locally" below.

## Phase 1 — Rewind + fork (1.5–2 d) ← the one you miss on a phone

Protocol already has it: `rewindSessionToMessage { turnCount }` and
`forkSessionFromResponse { turnCount }` (plus `rollback` / `fork` in
`Command`).

- Reference: web `apps/web/src/lib/runtime-context.tsx:1188-1250`; desktop
  `src/app/runtime.rs:230-340`.
- Mobile changes:
  - `apps/mobile/src/lib/daemon-api.ts` — add the two requests.
  - `apps/mobile/src/lib/runtime-context.tsx` — `rewindSession` / `forkSession`
    callbacks alongside the existing `renameSession` / `deleteSession`.
  - `apps/mobile/src/components/session-view.tsx` — add `rewind` and `fork` to
    `TASK_MENU_COMMANDS` (:59) and handle them in `handleTaskMenuCommand` (:209).
  - `apps/mobile/src/components/session-option-sheets.tsx` — a turn-count
    sheet (reuse the `ModelSheet` pattern).
- UX: message row long-press → "Rewind to here", destructive `Alert.alert`
  confirm (mirrors `confirmDelete`, session-view.tsx:183).
- **Status: implemented.** `daemon-api.ts` gained `rewindSessionToMessage` /
  `forkSessionFromResponse`; `runtime-context.tsx` gained `rewindSession` /
  `forkSession` (drop the followed runtime for rewind, cache the returned
  session, invalidate the session + task-state queries); `session-view.tsx`
  gained the two menu commands, the busy/empty guards and a confirm sheet;
  `session-option-sheets.tsx` gained `TurnSheet`; the pure turn list moved to
  `turnOptionsForSession` in `lib/session-presentation.ts` with four tests.
  `typecheck` and `bun test` (121) pass. Remaining: on-device run.
- Long-press on a message row was **not** added — the header menu is the only
  entry point so far. Add it later if the menu feels buried.
- **Gate on provider support**: only offer it when the provider can roll back
  (Rust: `ProviderKind::supports_conversation_rollback`; confirm whether that
  reaches the generated TS, otherwise derive from the provider list).
- **Gate on idle**: refuse while `is_busy()` — rewinding mid-turn races the
  daemon's per-session merge.
- Tests: port web's cases; `bun test`.

## Phase 2 — `/resume` (1 d)

- `listProviderSessions { provider, limit }` + `loadProviderSession
  { cursor, cwd }`.
- Reference: `apps/web/src/components/waku-app.tsx:658`,
  `apps/web/src/components/command-palette.tsx:188`,
  `apps/web/src/lib/daemon-api.ts:134-152`.
- **Status: implemented.** `daemon-api.ts` gained `listProviderSessions`,
  `loadProviderSessionHistory`, `sameProviderSession`, `providerSessionNativeId`,
  and `createResumedSession` (ported from web). `runtime-context.tsx` gained
  `resumeProviderSession`, which returns an existing task when the provider
  session is already tracked, otherwise creates the project if needed, copies
  the history, and caches the new task. `new-task.tsx` got a "Resume external
  session" row that opens `ResumeSessionSheet` (provider step → session list,
  refetched on reconnect, dedupe-safe); picking one navigates to the imported
  task. `daemon-api.test.ts` covers listing, native-id matching, and the
  resumed-task shape. `typecheck` clean, `bun test` 124 pass. Remaining:
  on-device run.

## Phase 3 — Usage (1–2 d)

- `loadUsageHistory` + `fetchPlanUsage`. `context-gauge.tsx` already renders
  occupancy; port web's usage page for spend/plan.
- **Status: implemented.** `daemon-api.ts` gained `loadUsageHistory` and
  `fetchPlanUsage` with `daemonKeys.usage` / `daemonKeys.planUsage`;
  `use-daemon-data.ts` gained `useUsageHistory` and `usePlanUsages` (one query
  per plan-capable provider, all disabled until settings land). The pure
  formatting and ranking rules live in `lib/usage-presentation.ts` with 19
  tests: window keys, money/token/percent compaction, the UTC range, and the
  plan reset countdown. `app/usage.tsx` is a new screen (window picker, spend
  card, provider bars, top models, plan lanes, scan footer) reached from the
  Usage bar button on the home and new-task screens; the route is registered
  inside `Stack.Protected` in `app/_layout.tsx`. `typecheck` clean, `bun test`
  143 pass. Remaining: on-device run.
- Deliberately not ported: web's daily/monthly/project views, the trend chart,
  and the cost-quality panel. A phone gets one window at a time and the
  summary plus top models answers the question it is opened for.
- Plan usage passes `cliVersion: null` — web forwards the probed CLI version,
  but mobile only probes with `probeVersion: false`. Revisit if a provider's
  plan output turns out to be version-dependent.

## Phase 4 — Small stuff (1–1.5 d)

- **Slash commands**: `apps/mobile/src/lib/mobile-runtime.ts:106` hardcodes
  `available_commands: []` — hydrate them and wire composer autocomplete.
- **Attachment preview**: `readAttachment` / `readBlob` are unused, so images
  never render; add to `transcript-rows.tsx` / `md-block-row.tsx`.
- **Transcript search**: `searchSessionMessages`.

## Phase 5 — Later / optional

Skills page, goal, settings (`updateSettings`), and distribution
(`eas.json` + TestFlight — today it's a USB dev build).

## Building locally (chosen over EAS and CI — run after the phases)

No EAS, no Android Studio IDE. Prerequisites, once:

- **JDK 17** (Temurin).
- **Android SDK command-line tools** (~2 GB), then:
  `sdkmanager "platforms;android-35" "build-tools;35.0.0" "platform-tools"`
- `bun install` at the repo root. **Windows gotcha:** `expo-libghostty`'s
  postinstall unzips its iOS framework and shells out to `unzip`, which Windows
  does not ship — `bun install` fails with `ENOENT: spawnSync unzip`. Either put
  Git's `C:\Program Files\Git\usr\bin` on `PATH` (worked here) or run
  `bun install --ignore-scripts`, since the iOS framework is not needed to
  build Android or to typecheck.

Day-to-day loop (phone over USB, or Wi-Fi via `adb pair` / `adb connect`):

```sh
bun --filter @waku/mobile android      # prebuild -> gradle -> install -> Metro
```

Afterwards JS changes reload over Metro — no rebuild.

Sideloading an APK instead of a cable:

```sh
npx expo prebuild --platform android
cd apps/mobile/android
.\gradlew assembleDebug                # app\build\outputs\apk\debug\app-debug.apk
adb install app\build\outputs\apk\debug\app-debug.apk
```

Notes:

- A **debug** APK expects Metro. Keep the PC and phone on the same Wi-Fi, or
  `adb reverse tcp:8081 tcp:8081` over USB. Add `expo-dev-client` (documented,
  not added yet) so the app opens a launcher where you scan the QR or type
  `http://<pc-ip>:8081` — that is what makes the loop feel native.
- A **standalone** APK (no PC running) needs `assembleRelease` plus a keystore.
- **iOS is not buildable here** (needs macOS + Xcode); EAS or a cloud Mac only.
- Metro runs on the PC; the Waku daemon runs on the desktop host. The phone is
  only a client — expose the daemon on the LAN IP with a fixed port, and
  `ws://` is fine because `app.json` sets `usesCleartextTraffic: true`.
- CI can still produce an APK later (a `workflow_dispatch` job with
  `setup-java` + `android-actions/setup-android` + `gradlew assembleDebug` +
  `upload-artifact`), pushed to the `yaffalhakim1/waku` fork — upstream
  (`egoist/waku`) is never touched.

## Risks

1. **Concurrent edits.** `SaveTaskState` is last-writer-wins with a guard only
   against a superseded runtime. Rewinding from the phone while the desktop is
   streaming is the realistic collision — hence the idle gate in Phase 1.
2. **Rewind is destructive and provider-dependent** (not every provider can
   truncate history). Gate, confirm, and disable rather than fail late.
3. **No CI until Phase 0** — land that first or the rest rots.

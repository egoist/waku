# `@oai/sky` macOS accessibility implementation

This document is a semantic reconstruction of the macOS window-control path
shipped in `@oai/sky` 0.6.2. It is intended to be precise enough to implement a
compatible client and native controller. It is not a claim that the original
Swift source was recovered byte-for-byte.

## Artifact and evidence

The inspected package is:

```text
/Applications/ChatGPT.app/Contents/Resources/cua_node/lib/node_modules/@oai/sky
```

The native service is also installed at:

```text
~/.codex/computer-use/Codex Computer Use.app
```

Artifact identity:

| Item | Value |
| --- | --- |
| npm package | `@oai/sky` 0.6.2 |
| service bundle | `com.openai.sky.CUAService` |
| service version | `26.727.1000550` (`1000550`) |
| architecture | arm64 |
| minimum macOS | 14.4 |
| team ID | `2DC432GLL2` |
| executable SHA-256 | `bbf2b878b2c1b1d5d7c0b7184443cd688952801a03094c276f82b734f90ea777` |
| client protocol | `CodexComputerUseIPC-2` |

The bundled and installed service executables were byte-identical when this
analysis was performed.

The package layout also makes the platform boundary explicit (**JS/package
exact**):

```text
@oai/sky/
  README.md
  docs/sky-window-api.md          # macOS
  docs/sky-window2-api.md         # Windows
  docs/sky-full-desktop-api.md    # Linux
  dist/project/cua/sky_js/src/    # model-facing clients and transports
  Codex Computer Use.app/         # signed macOS native service
  bin/linux/                      # arm64 and x64 native helpers
  bin/windows/                    # arm64 and x64 native helpers
```

The macOS service bundle also embeds `SkyComputerUseClient.app`,
`CUALockScreenGuardian.app`, and a separate installer helper under
`Contents/SharedSupport`. The top-level service is an `LSUIElement` app, so it
does not acquire a normal Dock presence. Its `Info.plist` identifies macOS
14.4 as the deployment floor and macOS 26.1 as the build SDK.

Evidence labels used below:

- **JS exact**: readable emitted JavaScript or TypeScript declarations.
- **ABI exact**: demangled exported Swift symbol and signature.
- **Assembly exact**: confirmed from arm64 instructions and constants.
- **Inferred**: behavior reconstructed from call sites, state, strings, or
  surrounding control flow; the native source was not bundled.

## What is actually shipped

The npm package is a model-facing adapter. On macOS it does not implement
Accessibility in JavaScript. It validates inputs, applies app policy, and sends
JSON-RPC requests to a separately signed Swift service. The service statically
contains modules named `ComputerUse`, `ComputerUseClient`,
`AccessibilitySupport`, `SystemSoftware`, `SlimCore`, and others.

No Swift source, `.swiftinterface`, dSYM, or source map is included. The native
executable does retain unusually rich Swift exports. There are 3,249 exported
symbols containing `AccessibilitySupport.`, which makes a high-confidence
semantic reconstruction possible without pretending the original source is
available.

## Public macOS API

The supported macOS client has these methods (**JS exact**):

```ts
interface WindowComputerUseClient {
  target: "mac"
  list_apps(): Promise<ListAppsApp[]>
  get_app_state(input: { app: string; disableDiff?: boolean }): Promise<AppState>
  click(input: {
    app: string
    element_index?: number
    x?: number
    y?: number
    click_count?: number
    mouse_button?: "left" | "right" | "middle" | "l" | "r" | "m"
  }): Promise<void>
  drag(input: {
    app: string
    from_x: number
    from_y: number
    to_x: number
    to_y: number
  }): Promise<void>
  press_key(input: { app: string; key: string }): Promise<void>
  type_text(input: { app: string; text: string }): Promise<void>
  scroll(input: {
    app: string
    element_index: number
    direction: "up" | "down" | "left" | "right" | "u" | "d" | "l" | "r"
    pages?: number
  }): Promise<void>
  set_value(input: { app: string; element_index: number; value: string }): Promise<void>
  perform_secondary_action(input: {
    app: string
    element_index: number
    action: string
  }): Promise<void>
  select_text(input: {
    app: string
    element_index: number
    text: string
    prefix?: string
    suffix?: string
    selection_type?: "text" | "cursor_before" | "cursor_after"
  }): Promise<void>
}
```

The lower-level `MacComputerUseClient` additionally exposes `getAppPolicy()`
and `startApp()`. The public facade initializes lazily on the first real call,
not on import.

### Validation and wire actions

The client validates finite coordinates, integer element indexes, positive
scroll pages, known mouse buttons/directions, and non-empty key strings. It
removes properties whose value is `undefined` before serialization.

The exact native action payloads are:

```json
{"click":{"at":{"elementID":{"_0":"42"}},"clickCount":1,"mouseButton":0}}
{"click":{"at":{"coordinate":{"_0":[120,80]}},"clickCount":1,"mouseButton":1}}
{"drag":{"from":[20,30],"to":[200,220]}}
{"performSecondaryAction":{"action":"AXShowMenu","elementID":"42"}}
{"pressKey":{"_0":"CMD+SHIFT+P"}}
{"scroll":{"at":{"elementID":{"_0":"42"}},"direction":"down","pages":1}}
{"setValue":{"elementID":"42","value":"replacement"}}
{"selectText":{"elementID":"42","text":"needle","prefix":null,"suffix":null,"selection":"text"}}
{"type":{"_0":"text"}}
```

Mouse buttons map to `0 = left`, `1 = right`, and `2 = middle`.

## Policy and target canonicalization

Every public app-scoped method except `list_apps` follows this sequence
(**JS exact**):

1. Send `ComputerUseIPCAppPolicyRequest` for the supplied app identifier.
2. Receive an `allowed`, `denied`, or `forbidden` policy decision and a
   canonical target containing `appPath`, bundle identifier, display name,
   risk, and optional warning subtitle.
3. Ask the host for approval, including session/always persistence when the
   policy permits it.
4. Replace the model-supplied `app` property with the approved canonical
   `appPath` in a frozen copy of the input.
5. Send the real request and record tool telemetry.

This is a security boundary. A compatible implementation must not approve one
identifier and then act on the original uncanonicalized string.

## Native transport

The default socket is (**JS exact**):

```text
~/Library/Group Containers/2DC432GLL2.com.openai.sky.CUAService/IPC/computeruse.sock
```

`SKY_CUA_SERVICE_NATIVE_PIPE_PATH` can override it. If the first 250 ms
connection attempt fails, the client asks the trusted host runtime to ensure
the service, or opens the service through Launch Services, then retries for up
to five seconds.

The startup fallback chain is readable in `native-pipe.js` (**JS exact**):

1. If `NODE_REPL_HOST_SERVICES_PIPE_PATH` is present, send an `ensureService`
   JSON-RPC request with `{ "service": "computer-use" }` to the trusted host.
2. Otherwise use the trusted runtime's `launchServices.openApplication`.
3. Prefer `SKY_CUA_SERVICE_PATH` when set.
4. Otherwise prefer `$CODEX_HOME/computer-use/Codex Computer Use.app` when it
   exists.
5. Otherwise launch bundle identifier `com.openai.sky.CUAService`.

`OAI_SKY_CONFIG_PATH` belongs to the package-level target selector. If set, its
JSON replaces the default platform-derived `{ "target": "mac" }` config.
`post_action_sleep_ms`, also documented in that config, is consumed by the
Linux full-desktop backend in this artifact; it is not evidence of a macOS
post-action delay.

Frames are:

```text
uint32 little-endian payload length
UTF-8 JSON payload
```

The maximum payload is 8 MiB. The connection uses JSON-RPC 2.0 with monotonically
increasing numeric IDs. Requests are serialized on one persistent connection
per API version. A connection is discarded after closure or transport failure.

RPC methods:

- `ping({ clientApiVersion })`
- `request({ clientApiVersion, codexTurnMetadata, deadlineUnixMilliseconds,
  requestType, request })`

Native request type names:

- `ComputerUseIPCAppPolicyRequest`
- `ComputerUseIPCAppGetSkyshotRequest`
- `ComputerUseIPCListAppsRequest`
- `ComputerUseIPCAppPerformActionRequest`
- `ComputerUseIPCAppStartRequest`

The server error range is `-10000...-10020`, covering unauthenticated sender,
bad request type/data, unhandled event, policy denial, missing application,
Accessibility/permission errors, inactive session, stop/intervention,
incompatible version, pending permission, blocked URL, ambiguous app, missing
bootstrap port, and locked screen.

## Native session/controller

Each selected process is represented by a long-lived
`ComputerUseAppController` (**ABI exact**). Its retained state includes:

- canonical application target, bundle ID, `NSRunningApplication`, and PID;
- `ApplicationUIElement` and a window-ID-to-AX-window map;
- last selected window and last `RefetchableSkyshotAXTree`;
- tree-diff enablement and transformation cache;
- accessibility enablement assertion;
- one retained `SyntheticAppFocusEnforcer`;
- window ordering and AX notification observers;
- currently open menu and focused menu-bar item locks;
- scaling, visible geometry, virtual cursor, and screenshot files.

Important lifecycle methods:

```text
activate()
deactivate()
activateFocusEnforcer() -> SyntheticAppFocusEnforcer
updateSkyshot(treeCache:disableAXDiffing:skipScreenshot:)
prepareToInteract(with:cursorNextInteractionTiming:positionElement:)
```

The focus enforcer is created lazily and retained by the controller. It is not
created and destroyed around every pointer event.

## App state / Skyshot pipeline

The state response is a `skyshot` containing formatted Accessibility text and
an optional screenshot data URL. The top-level path is (**ABI exact**):

```text
FocusedUIElementContext
  -> SystemSelectionExtractor.extract(...)
  -> SkyshotOperation.captureAXTree(...)
  -> SkyshotOperation.capture(...)
  -> ComputerUseSkyshotAttachment
```

### Window and context resolution

`FocusedUIElementContext.contextForAppName` and
`NSRunningApplication.focusedUIElementContext()` resolve an application,
AX window, matching CoreGraphics window, and focused element. The service keeps
window-order observations because the Accessibility hierarchy alone is not a
complete z-order model.

`ApplicationWindow.keyWindow(in:)`, AX focused/main window attributes, window
IDs, role/subrole, geometry, visibility/minimization, and CoreGraphics window
metadata participate in matching (**ABI exact**, selection weights inferred).

### AX enablement

`AXEnablementAssertion` is retained for the controller/session. Native exports
confirm an explicit assertion with `disable()` and deinit cleanup. Strings and
call sites confirm the Chromium/Electron compatibility attributes
`AXManualAccessibility` and `AXEnhancedUserInterface` (**inferred**).

This is not equivalent to setting those attributes once and forgetting them;
the assertion has a lifetime and teardown path.

### Tree construction

The service wraps raw AX objects in typed elements including application,
window, sheet, web area, menu bar, menu-bar item, menu, menu item, link, scroll
area, scroll bar, and editable text elements.

The generic `UIElementTree` supports (**ABI exact**):

- depth-first traversal;
- `IndexPath` and numeric element-ID lookup;
- raw-to-`TransformedUIElement` conversion;
- focused subtrees;
- menu-bar insertion;
- filtering and visible-child subsets;
- URL trimming;
- rendering into a line-oriented `UIElementRenderTree`;
- revisions and render differences.

The transformation layer materializes model-relevant fields such as role,
subrole, label/title, description, value, enabled/focused/selected state,
geometry, actions, child subsets, and backing AX element. The native library
exports 576 `UIElementAttribute` symbols, 274 role symbols, 121 action symbols,
and 161 notification symbols.

Large child arrays can be represented by `AXArraySubsetDescriptor` and
`AXTruncationDescriptor`; the tree exposes
`visibleChildrenIfChildrenExceedsThreshold`. URLs are compacted with
per-URL/total limits and query/fragment controls.

### Stable indexes and refetch

The numeric element index shown to the model is a session/tree identity, not a
screen coordinate. Before acting, the controller calls `prepareToInteract` and
the `RefetchableSkyshotAXTree` can:

- refetch the tree;
- refetch an element by index;
- validate that the element remains actionable;
- position/scroll the element before interaction.

Therefore, stale indexes are intentionally rejected or refetched. A compatible
implementation must bind indexes to app, PID, window, and tree revision.

### Diffing

The first state is a root `UIElementTreeRevision`. Later states append a new
tree and compute `UIElementRenderDifference` changes. Changes retain path,
offset, depth-first order, and can inherit element IDs so unchanged controls do
not get arbitrary new indexes. `disableDiff` forces a full tree.

The response can also include a focused-tree snapshot and an Accessibility
inspector payload. The exact human-readable line grammar is produced by
`LMReadableElement.lmReadableDescription` and render-line options; no formatter
source is shipped, so grammar parity must be tested against captured outputs.

### Screenshot composition

The screenshot path uses ScreenCaptureKit/CoreGraphics window geometry and can
include related transient windows. It tracks image size, scale factor, visible
rect, opacity, shadow behavior, and virtual cursor. The JS facade exposes the
native screenshot URL as `{ url: dataURL }`.

App-specific instructions are prepended once per bundle/app identity for the
lifetime of the JS client. Numbers (`com.apple.iWork.Numbers`) is explicitly
excluded from this prefixing behavior.

## Action dispatch

The controller exposes separate element and coordinate paths (**ABI exact**):

```text
click(elementID:type:numberOfClicks:returnSkyshot:)
click(at:with:clickCount:andDragTo:returnSkyshot:)
performSecondaryAction(elementID:action:returnSkyshot:)
setValue(elementID:value:returnSkyshot:autosubmitSearchFields:)
selectText(elementID:text:prefix:suffix:selection:returnSkyshot:)
performKeyboardAction(...)
scroll(deltaX:deltaY:)
```

### Element click

An element click refetches and validates the indexed element, obtains a
clickable point (optionally scrolling it visible), determines whether it is
inside a web view, resolves the relevant window, and chooses AX action versus
synthetic pointer delivery. `alwaysSimulateClick` exists on the lower-level
element API, so AXPress is not universally used.

The app wrapper has explicit detection for Web, Catalyst, and Electron targets
and can resolve an out-of-process target/window. This matters for browser and
ViewBridge controls whose AX element belongs to a helper process.

### Coordinate click and drag

Coordinate actions scale screenshot-local points into screen coordinates and
resolve a `MouseEventTarget` containing PID, optional AX element/window, window
ID, bounds, and flipped-coordinate state.

The synthetic mouse event is created as an `NSEvent`, converted to `CGEvent`,
then amended (**assembly exact**):

- field `3` = mouse button;
- field `7` = mouse subtype `3`;
- fields `91` and `92` = target window ID when window-targeted;
- global location is set;
- private `CGEventSetWindowLocation` sets the window-local location.

Events are posted directly to the target PID. Matching mouse-down/up events
share an event number. Drag samples share a second event number.

### Keyboard and text

`ApplicationUIElement.targetForKeyboardEvent()` resolves the correct process,
including out-of-process content. Key chords are parsed into virtual key
presses. `SynthesizedEvent.pressKeys`, `pressKeysForHolding`, and
`type(string:)` generate PID-targeted events. Holding-key support returns
separate key-down and key-up bundles.

### Set/select text

Editable controls use value/selected-range/marker APIs rather than assuming one
AX attribute. `EditableTextUIElement` exports replacement and insertion
operations. Selection supports contextual prefix/suffix disambiguation and
cursor-before/cursor-after modes.

### Secondary actions and menus

Secondary actions are resolved from the element's advertised AX actions. Menu
state is tracked explicitly through AX menu-open/menu-close/menu-item-selected
notifications and the controller's current-menu locks.

Context menus are not treated as a normal right-click followed by an unrelated
snapshot. While a menu is opening, the focus enforcer can call
`startSuppressingMenuDismissalEvents(menuPID:)`; suppression remains active for
the menu process and is stopped separately. This is required for transient
menus hosted by another process.

## Exact background-focus architecture

This is the critical path for Waku parity.

Sky does not activate the target application through
`NSRunningApplication.activate`. It maintains two parallel truths:

| State | Meaning |
| --- | --- |
| `applicationBelievesItIsActive` | synthetic AppKit state delivered to target |
| `applicationBelievesItHasFocus` | synthetic key-focus state delivered to target |
| `applicationIsActive` | real frontmost state observed from the system |

`SyntheticAppFocusEnforcer(pid:)` initializes those values from the running
application and `SystemFocusStealPreventer.isAppCurrentlyFocused(pid)`. It
registers lost/gained-focus callbacks and a frontmost-app observer.

### Enforce transition

`enforceActiveState(for: window)` performs this state machine (**assembly
exact**):

1. If the target already believes it has focus, send nothing.
2. If it believes it is active but lacks focus, send only
   `notifyWindowKeyFocusReturned`, then mark believed focus true.
3. Otherwise send `notifyAppActivated` for the target window, mark believed
   active true, then send `notifyWindowKeyFocusReturned`, and mark believed
   focus true.

The order is activation first, key-focus-returned second.

### Exact synthetic focus packets

The packet constructors are (**assembly exact**):

| Packet | NSEvent type | subtype | window | flags |
| --- | ---: | ---: | ---: | ---: |
| app activated | 13 | 1 | target window ID | `0xC0000` when window ID is nonzero |
| app deactivated | 13 | 2 | 0 | 0 |
| window key focus returned | 21 | `0x8000` | 0 | 0 |
| window key focus removed | 21 | `0x4000` | 0 | 0 |

The activated event has zero context/data fields. When the target window has
usable, non-empty, unflipped geometry, `notifyAppActivated` also bundles a
synthetic left-mouse-down and left-mouse-up at the activation point. Those
events carry the same target window ID, fields `3 = 0`, `7 = 3`, fields
`91/92 = windowID`, and a window-local location.

Sending only the activation packet is unsafe. It is one component of a
retained focus-enforcement session.

### System focus-steal preventer

`SystemFocusStealPreventer` is process-global and retains a locked array of
`DisallowedThiefProcess` entries. Each entry contains:

- target PID;
- lost-focus and gained-focus callbacks;
- optional per-PID mouse event taps;
- menu-dismissal suppression state.

It owns:

- a system process-notification event tap;
- a ViewBridge keyboard event tap;
- private current-key-focus queries and focus-release/set-front-process calls.

The process-notification parser recognizes (**assembly/string exact**):

| Notification | subtype |
| --- | ---: |
| NewFront | `2` |
| LostKeyFocus | `0x1000` |
| KeyFocusTaken | `0x4000` |
| KeyFocusReturned | `0x8000` |
| KeyFocusChanged | `0xF102` |
| LostTypingFocus | `0xF105` |
| TypingFocusChanged | `0xF107` |

It also decodes private event fields named `focusTheftID` and
`focusThiefAlsoStoleTypingFocus`.

`isAppCurrentlyFocused` first consults the installed preventer state. Without
that state, it queries the private current key-focus process and falls back to
the workspace frontmost PID.

### The second tap

When `applicationBelievesItIsActive && !applicationIsActive`, the enforcer
installs an additional head-insert annotated-session tap for private event type
`32` (mask `1 << 32`) (**assembly exact**). It is removed when that predicate
becomes false.

The callback ignores the service's own events, reads source and target PIDs,
decodes the focus-theft identity, and routes relevant events through the
process-global preventer (**assembly exact up to the private field decoder**).
This tap is not an optional debounce. It is the guard that makes a target
believe it is active while preventing that belief from becoming a real
WindowServer activation.

### Action completion and teardown

`synthesizedActionWasPerformed()` records system uptime and PID in shared
state. The focus callbacks use this to distinguish an immediate consequence of
synthesized input from genuine user intervention (**inferred from state and
call sites**).

`deactivateFocusEnforcer()` is separate from action completion. Temporary
enforcers are created only when a call site has no retained controller
enforcer; those temporary instances are deactivated after the click. Normal
controller actions reuse the retained instance and deactivate it when the
controller/session ends.

## Why Waku's previous approximation failed

The prior implementation diverged in four material ways:

1. It created AX synthetic focus and a focus event tap around each action,
   restoring everything after roughly 50 ms. Sky retains its focus enforcer
   across controller actions.
2. It sent key-focus-returned before activation. Sky sends activation first.
3. It changed the activation packet to window `0` with no `0xC0000` flags after
   observing foregrounding. Sky does use the window ID and `0xC0000`; safety
   comes from the already-armed preventer and private event-type-32 tap.
4. It treated context menus as ordinary clicks. Sky retains menu-dismissal
   suppression and tracks transient menu PIDs/windows independently.

These differences explain the observed combination: either Kero receives too
little logical activation and opens the wrong menu, or it receives an
activation-shaped event without Sky's guard and comes to the front.

## Compatibility requirements for Waku

A parity implementation must satisfy all of the following as one unit:

- one retained focus enforcer per controlled PID/session;
- separate believed-active, believed-key-focus, and actual-frontmost state;
- install the process-notification preventer before synthetic activation;
- install the private event-type-32 tap whenever synthetically active but not
  actually frontmost;
- send window-scoped subtype-1 activation with `0xC0000`, then subtype-`0x8000`
  key-focus-returned;
- use PID-targeted window-aware NSEvents with global and local locations;
- bind element IDs to app/PID/window/tree revision and refetch before acting;
- avoid indiscriminately setting AXFocused on the element under a coordinate;
- retain menu-dismissal suppression while a contextual menu is open;
- preserve the user's real frontmost app and first responder;
- treat a real user focus change as authoritative intervention;
- tear down synthetic focus only at controller/session deactivation.

The acceptance test for the reported bug is stricter than “the foreground app
is restored afterward”:

1. Codex/Waku remains the actual frontmost application throughout.
2. The Kero terminal cursor changes from outline to solid.
3. A right-click on the intended project row opens that row's context menu.
4. The menu contains Kero's project actions (`Rename…`,
   `Set Project Directory…`, and `Close Project`).
5. No real input field in the frontmost app loses first-responder focus.

## Known limits of this reconstruction

- The original Swift source and private field-name resolution tables are not
  shipped. Private CPS/CGS calls can be identified by behavior and call shape,
  but not assigned source-level declarations with complete certainty.
- The formatter's exact line grammar, every role-specific transformation, and
  every one of the 3,249 exported Accessibility symbols are not reproduced
  line-by-line here. Their architecture and action-critical paths are covered.
- Private event fields and event type `32` are macOS implementation details and
  can change between service builds. Compatibility must be pinned to an
  artifact version and exercised on every supported macOS release.
- Linux and Windows use different native backends and are outside this
  macOS Accessibility reconstruction.

## Reproduction commands

```sh
SKY='/Applications/ChatGPT.app/Contents/Resources/cua_node/lib/node_modules/@oai/sky'
BIN="$HOME/.codex/computer-use/Codex Computer Use.app/Contents/MacOS/SkyComputerUseService"

cat "$SKY/package.json"
shasum -a 256 "$BIN"
xcrun dyld_info -exports "$BIN" | xcrun swift-demangle
xcrun llvm-objdump --arch=arm64 --disassemble \
  --start-address=0x1006A8588 --stop-address=0x1006AA000 "$BIN"
```

Relevant exported entry points in this artifact:

```text
0x100066178  ComputerUseAppController.updateSkyshot
0x100069120  ComputerUseAppController.activateFocusEnforcer
0x100069E14  ComputerUseAppController.prepareToInteract
0x10006BD50  ComputerUseAppController.click(elementID:...)
0x10006D730  ComputerUseAppController.performSecondaryAction
0x100070C0C  ComputerUseAppController.performKeyboardAction
0x100076A54  ComputerUseAppController.click(at:...)
0x1001F0358  SystemSelectionExtractor.extract
0x1001EBB44  SkyshotOperation.captureAXTree
0x1006A8588  SyntheticAppFocusEnforcer.init
0x1006A8B98  SyntheticAppFocusEnforcer.enforceActiveState
0x1006A94C4  SyntheticAppFocusEnforcer.deactivateFocusEnforcer
0x1006A97D0  SyntheticAppFocusEnforcer.startSuppressingMenuDismissalEvents
0x1006A9B90  SyntheticAppFocusEnforcer.synthesizedActionWasPerformed
0x1006B3B54  SynthesizedEvent.notifyAppActivated
0x1006B399C  SynthesizedEvent.notifyWindowKeyFocusReturned
0x1006B3FB8  SynthesizedEvent.mouseEvent
0x100677DE4  ApplicationUIElement.syntheticallyActivateIfNeededForSendingClick
0x10067836C  ApplicationUIElement.sendClick
0x100753984  UIElementProtocol.click
```

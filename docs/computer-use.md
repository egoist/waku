# Computer Use

Waku embeds **Cua Driver 0.28.0** through its native SDK ABI (1.1). The
JavaScript REPL exposes every native tool directly, such as `cua.list_apps()`,
`cua.get_window_state(args)`, and `cua.click(args)`. Setup binds the native methods internally. The bundled skill contains the
host-specific method signatures and direct-call examples; agent code does
not discover or dispatch tools through a catalog API.
Use `jsRepl.write(value)` for output and `await jsRepl.emitImage(image)` for
images. Tool schemas, capture, accessibility, input, and authorization come
from Cua.
The previous `sky` API and custom macOS action engine have been removed.
Computer Use is a development-only feature. Debug builds expose its settings
page and honor the enable flag; release builds clamp that flag before any
driver starts, and the REPL refuses to launch the helper, so no production
build registers the `cua` bridge, attaches the skill, or reaches the SDK.

## Processes and lifetime

On macOS, the signed `Waku Computer Use.app` hosts the SDK library directly.
Its Launch Services bridge preserves the helper's existing independent TCC
identity and Waku's Screen Recording/Accessibility onboarding. The bundled
library is signed with the same identity as the helper. Permission requests
remain host-owned: direct SDK permission checks do not open macOS prompts.

On Windows and Linux, `waku_computer_use` loads the packaged SDK library into
its own process. It communicates with the REPL over inherited stdin/stdout.
Windows also packages Cua's UIA support executable. No Cua daemon, installation,
Python runtime, Node runtime, or separately running service is required.

Each helper connection owns one SDK runtime. Native tool refusals preserve the
connection and the full result, including error codes, snapshot tokens, capture
metadata, and action outcomes. REPL reset and disconnect close the connection;
Stop cancels native work and ends the host. Interrupted actions are never
automatically retried. The `bring_to_front` tool is omitted from the exposed
API. Other tool arguments pass through to Cua unchanged. All SDK and IPC work
occurs outside the GUI process.

Waku's preview decodes each PNG from the agent's `get_window_state` result on
a background worker. The previous decoded frame stays visible until the latest
replacement is ready; stale or invalid frames are discarded. There is no
second capture or continuous accessibility walk to change the agent's snapshot.

The helper initializes Cua's native cursor facility. On macOS, Cua's renderer
owns the helper's OS main thread while MCP and actions run on workers. Windows
and Linux use Cua's native overlay thread. Cursor movement, action animations,
themes, and reduced-motion handling use the same implementation as standalone
Cua Driver. Headless hosts still report unavailable graphics facilities.

## OpenCode 2

OpenCode 2 uses the existing shared service. Waku registers one temporary MCP
connection per workspace through `/api/mcp` and attaches a session instruction
pointing to the bundled skill (OpenCode limits each entry to 8 KB). `js` and `js_reset` remain direct tools, with
OpenCode's additional codemode wrapper disabled for this server.

OpenCode's `_meta.sessionID` selects a Waku-owned registration, so each task
has independent JavaScript bindings, native helper processes, cancellation,
and PiP frames. Unregistered sessions cannot execute calls through the bridge.
Detaching a task revokes its registration and removes its instructions; the
last task removes the temporary MCP server. Reconnecting checks the live
server before replacing it, preserving kernels across ordinary SSE reconnects.
No OpenCode configuration files or service descriptors are written.

## Platform requirements

- **macOS:** grant the Waku helper Screen Recording and Accessibility access
  in Settings > Computer Use. Relaunch the permission-owning helper after a
  grant changes; new REPL connections launch a fresh helper.
- **Windows:** run within the user's interactive desktop. Elevated apps and
  secure desktops remain subject to Windows restrictions. The SDK's native
  capability/error results describe supported input routes.
- **Linux:** X11 uses the active display and AT-SPI accessibility services.
  In a Wayland session, Waku enables Cua's experimental native Wayland backend
  unless `CUA_DRIVER_RS_ENABLE_WAYLAND` is already set. Window targeting and
  input depend on the compositor's supported routes and installed desktop
  integrations. Cua's GNOME helper files ship under
  `share/waku/computer-use/wayland-helper`; Waku does not automatically install
  shell extensions or compositor plugins. `check_permissions` and the native
  tool catalog describe what is available. Unsupported background delivery
  remains an explicit refusal.

Native window IDs are preserved as 64-bit values, including in preview events.
Use IDs and element tokens from fresh observations rather than reconstructing
them or assuming discovery order implies focus.

## Packaging and checks

`scripts/cua-driver.ts` pins release support artifacts and SHA-256 checksums
for macOS, Windows, and Linux on x64 and ARM64. `scripts/cua-host.ts` builds the
SDK from the same pinned source revision with its native host entrypoints
exposed through `resources/computer-use/cua-host.rs`. This small ABI extension
enables Cua's existing cursor facility and main loop; it does not implement
input, capture, or rendering. Authorization still uses Cua's original checks.

The SDK uses its own pinned Rust toolchain and lockfile, isolated from Waku's
workspace. Sources and builds are cached under `.waku-cache/cua-host` so normal
dev rebuilds reuse the compiled SDK. The macOS bundle, Windows installer/zip,
Linux tarball, and dev watcher package the same host-enabled SDK. `scripts/cua-api.ts` reads the native tool
metadata during packaging and writes the complete API reference into the
bundled skill. Bump the version, all platform checksums, and
the ABI bindings together. Include `resources/computer-use/CUA-LICENSE`.

The portable host can run `list-tools` as a diagnostic without capturing or
operating the desktop. The protocol smoke test uses only tool discovery,
configuration reads, a request missing required arguments, a synthetic image,
and REPL reset/reconnect:

```sh
cargo build -p waku --bin waku_js_repl -p waku-computer-use --bin waku_computer_use
bun scripts/cua-driver.ts bundle target/debug target/debug/resources debug
bun scripts/test-computer-use.ts
```

To test the signed macOS host, pass its packaged REPL and helper executable
paths to `scripts/test-computer-use.ts`. Add `--expect-cursor` to check native
cursor availability in a graphical session without moving or clicking anything.
The CI matrix runs the portable SDK
smoke test on all three operating systems. UI automation tests are separate
and should only run when requested.

References: [in-process SDK guide](https://cua.ai/docs/how-to-guides/driver/use-sdk-in-process),
[native ABI](https://github.com/trycua/cua/blob/1b50c02e2d34734f64d2d22f54eb76cc97b4a663/libs/cua-driver/rust/include/cua_driver_abi.h),
[pinned release](https://github.com/trycua/cua/releases/tag/cua-driver-rs-v0.28.0).

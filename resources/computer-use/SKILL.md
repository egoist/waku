---
name: waku-computer-use
description: Control local macOS, Windows, and Linux apps through Waku Computer Use. Prefer purpose-built connectors, APIs, or CLIs when available.
---

# Waku Computer Use

Use the `js` tool from `waku_js_repl` for computer interactions. It runs a
persistent QuickJS kernel. Use the direct `cua` methods documented here;
all methods are available immediately after bootstrap. Waku shows Cua's native
virtual cursor automatically while actions run; no cursor setup is needed.

## Bootstrap

Run once per fresh JavaScript session:

```js
if (!globalThis.cua) {
  await setupComputerUseRuntime({ globals: globalThis });
}
```

After `js_reset`, bootstrap again before using `cua`. Module imports and Node
subprocess APIs are unavailable. The native runtime is managed by Waku.

Use `jsRepl.write(value)` for text or structured output and
`await jsRepl.emitImage(image)` to show a returned image. Prefer top-level `var`
for names reused across calls. Calls default to 30 seconds; set `timeout_ms`
when an observation needs longer.

<!-- BEGIN NATIVE API -->
## API surface

Call these methods directly. Arguments use the native names shown below; conditional constraints are validated by the runtime.

```ts
type CuaResult = {
  content: Array<{ type: 'text'; text: string } | { type: 'image'; data: string; mimeType: string }>;
  structuredContent?: any;
  isError?: boolean;
};

declare const cua: {
  readonly platform: "macos";
  list_apps(args?: Record<string, never>): Promise<CuaResult>;
  list_windows(args?: { on_screen_only?: boolean; pid?: number }): Promise<CuaResult>;
  get_window_state(args: { capture_mode?: "ax" | "vision"; include_accessibility_tree?: boolean; include_screenshot?: boolean; max_depth?: number; max_dimension?: number; max_elements?: number; pid: number; query?: string; screenshot_out_file?: string; session?: string; window_id: number }): Promise<CuaResult>;
  verify_state(args: { expect: Array<{ element?: { enabled?: boolean | null; exists?: true; selected?: boolean | null; selector: { label_contains?: string; role?: string }; value_equals?: string | null } | null; window?: { bounds?: { height: number; tolerance_px?: number; width: number; x: number; y: number } | null; exists?: boolean | null } | null }>; include_screenshot?: boolean | null; pid: number; session?: string; stable_samples?: number; timeout_ms?: number; window_id: number }): Promise<CuaResult>;
  launch_app(args?: { additional_arguments?: Array<string>; bundle_id?: string; creates_new_application_instance?: boolean; name?: string; urls?: Array<string>; webkit_inspector_port?: number }): Promise<CuaResult>;
  kill_app(args: { pid: number }): Promise<CuaResult>;
  set_window_frame(args: { height: number; pid: number; session?: string; width: number; window_id: number; x: number; y: number }): Promise<CuaResult>;
  invoke_menu(args: { path: Array<string>; pid: number; session?: string; window_id: number }): Promise<CuaResult>;
  click(args?: { action?: string; button?: "left" | "right" | "middle"; count?: number; debug_image_out?: string; delivery_mode?: "background" | "foreground"; element_index?: number; element_token?: string; from_zoom?: boolean; modifier?: Array<string>; pid?: number; scope?: "window" | "desktop"; session?: string; snapshot_id?: string; target?: { kind: "window"; pid: number; window_id: number; [key: string]: unknown } | { display_id: string; kind: "desktop"; [key: string]: unknown }; window_id?: number; x?: number; y?: number }): Promise<CuaResult>;
  double_click(args: { delivery_mode?: "background" | "foreground"; element_index?: number; element_token?: string; pid: number; session?: string; snapshot_id?: string; window_id?: number; x?: number; y?: number }): Promise<CuaResult>;
  right_click(args: { delivery_mode?: "background" | "foreground"; element_index?: number; element_token?: string; modifier?: Array<string>; pid: number; session?: string; snapshot_id?: string; window_id?: number; x?: number; y?: number }): Promise<CuaResult>;
  drag(args: { button?: "left" | "right" | "middle"; delivery_mode?: "background" | "foreground"; duration_ms?: number; from_x: number; from_y: number; from_zoom?: boolean; modifier?: Array<string>; pid?: number; scope?: "window" | "desktop"; session?: string; steps?: number; target?: null; to_x: number; to_y: number; window_id?: number }): Promise<CuaResult>;
  type_text(args: { delay_ms?: number; delivery_mode?: "background" | "foreground"; element_index?: number; element_token?: string; pid?: number; scope?: "window" | "desktop"; session?: string; snapshot_id?: string; target?: null; text: string; window_id?: number; x?: number; y?: number }): Promise<CuaResult>;
  press_key(args: { delivery_mode?: "background" | "foreground"; element_index?: number; element_token?: string; key: string; modifiers?: Array<string>; pid?: number; scope?: "window" | "desktop"; session?: string; snapshot_id?: string; target?: null; window_id?: number; x?: number; y?: number }): Promise<CuaResult>;
  hotkey(args: { delivery_mode?: "background" | "foreground"; element_index?: number; element_token?: string; keys: Array<string>; pid?: number; scope?: "window" | "desktop"; session?: string; snapshot_id?: string; target?: null; window_id?: number; x?: number; y?: number }): Promise<CuaResult>;
  set_value(args: { element_index?: number; element_token?: string; pid: number; session?: string; snapshot_id?: string; value: string; window_id?: number }): Promise<CuaResult>;
  scroll(args: { amount?: number; by?: "line" | "page"; delivery_mode?: "background" | "foreground"; direction: "up" | "down" | "left" | "right"; element_index?: number; element_token?: string; pid?: number; scope?: "window" | "desktop"; session?: string; snapshot_id?: string; target?: null; window_id?: number; x?: number; y?: number }): Promise<CuaResult>;
  clipboard_read(args?: { include_text?: boolean; session?: string }): Promise<CuaResult>;
  clipboard_write(args?: { file_path?: string; image_path?: string; session?: string; text?: string }): Promise<CuaResult>;
  get_screen_size(args?: { session?: string }): Promise<CuaResult>;
  get_desktop_state(args?: { screenshot_out_file?: string; session?: string }): Promise<CuaResult>;
  get_cursor_position(args?: { session?: string }): Promise<CuaResult>;
  move_cursor(args: { cursor_id?: string; scope?: "window" | "desktop"; session?: string; target?: null; x: number; y: number }): Promise<CuaResult>;
  set_agent_cursor_enabled(args: { enabled: boolean; session: string }): Promise<CuaResult>;
  set_agent_cursor_motion(args: { arc_flow?: number | null; arc_size?: number | null; dwell_after_click_ms?: number | null; end_handle?: number | null; glide_duration_ms?: number | null; idle_hide_ms?: number | null; session: string; spring?: number | null; start_handle?: number | null; turn_radius?: number | null }): Promise<CuaResult>;
  set_agent_cursor_theme(args: { reduced_motion?: "auto" | "on" | "off"; session: string; theme_id: string }): Promise<CuaResult>;
  get_agent_cursor_state(args: { session: string }): Promise<CuaResult>;
  check_permissions(args?: { probe_direct_capture?: boolean; prompt?: boolean }): Promise<CuaResult>;
  health_report(args?: { include?: Array<string>; skip?: Array<string> }): Promise<CuaResult>;
  get_config(args?: Record<string, never>): Promise<CuaResult>;
  set_config(args?: { experimental_pip?: boolean; experimental_pip_geometry?: string; key?: string; max_image_dimension?: number; value?: unknown }): Promise<CuaResult>;
  get_accessibility_tree(args?: Record<string, never>): Promise<CuaResult>;
  zoom(args: { pid?: number; window_id: number; x1: number; x2: number; y1: number; y2: number }): Promise<CuaResult>;
  page(args: { action: "execute_javascript" | "get_text" | "query_dom" | "click_element" | "insert_text" | "type_keystrokes" | "enable_javascript_apple_events"; attributes?: Array<string>; bundle_id?: string; cdp_port?: number; css_selector?: string; javascript?: string; pid?: number; selector?: string; target_url_contains?: string; text?: string; user_has_confirmed_enabling?: boolean; window_id?: number }): Promise<CuaResult>;
  get_browser_state(args?: { continuation?: string; include_screenshot?: boolean; pid?: number; query?: string; scope_ref?: string; session?: string; snapshot_format?: "dom_refs_v1" | "semantic_v2"; tab_id?: string; target_id?: string; window_id?: number; [key: string]: unknown }): Promise<CuaResult>;
  browser_prepare(args?: { allow_launch?: boolean; pid?: number; profile?: { mode: "isolated_new" | "isolated_named"; name?: string }; session?: string; strategy?: { kind: "existing_profile" }; window_id?: number; [key: string]: unknown }): Promise<CuaResult>;
  browser_navigate(args: { session?: string; tab_id: string; target_id: string; url: string; [key: string]: unknown }): Promise<CuaResult>;
  browser_click(args: { input_route?: "trusted" | "dom_event"; ref?: string; session?: string; tab_id: string; target_id: string; x?: number; y?: number; [key: string]: unknown }): Promise<CuaResult>;
  browser_type(args: { mode?: "insert_text" | "keystrokes"; ref: string; replace?: boolean; session?: string; tab_id: string; target_id: string; text: string; [key: string]: unknown }): Promise<CuaResult>;
  browser_dialog(args: { action: "inspect" | "accept" | "dismiss"; delivery_mode?: "background" | "foreground"; dialog_id?: string; prompt_text?: string; session?: string; tab_id: string; target_id: string; [key: string]: unknown }): Promise<CuaResult>;
  browser_set_input_files(args: { files: Array<string>; ref: string; session?: string; tab_id: string; target_id: string; [key: string]: unknown }): Promise<CuaResult>;
  browser_download(args: { destination_root: string; ref: string; session: string; tab_id: string; target_id: string; [key: string]: unknown }): Promise<CuaResult>;
  browser_pointer(args: { action: "hover" | "right_click" | "double_click" | "scroll" | "drag"; delta_x?: number; delta_y?: number; destination_ref?: string; input_route?: "trusted" | "dom_event"; ref?: string; session: string; tab_id: string; target_id: string; to_x?: number; to_y?: number; x?: number; y?: number; [key: string]: unknown }): Promise<CuaResult>;
  start_recording(args: { output_dir: string; record_video?: boolean }): Promise<CuaResult>;
  stop_recording(args?: Record<string, never>): Promise<CuaResult>;
  get_recording_state(args?: Record<string, never>): Promise<CuaResult>;
  replay_trajectory(args: { delay_ms?: number; dir: string; stop_on_error?: boolean }): Promise<CuaResult>;
  install_ffmpeg(args?: { confirm?: boolean }): Promise<CuaResult>;
  start_session(args?: { capture_scope?: "auto" | "window" | "desktop"; cursor_theme?: { reduced_motion?: "auto" | "on" | "off"; theme_id: string; [key: string]: unknown } | null; session?: string; [key: string]: unknown }): Promise<CuaResult>;
  escalate_session(args: { detail?: string; reason: "ax_tree_pixel_mismatch" | "background_delivery_failed" | "foreground_ineffective" | "no_window_target" | "other"; session: string; [key: string]: unknown }): Promise<CuaResult>;
  get_session(args?: { session?: string; [key: string]: unknown }): Promise<CuaResult>;
  list_sessions(args?: { cursor?: string; limit?: number | null; [key: string]: unknown }): Promise<CuaResult>;
  get_session_state(args?: { session?: string; [key: string]: unknown }): Promise<CuaResult>;
  end_session(args?: { session?: string; [key: string]: unknown }): Promise<CuaResult>;
};
```
<!-- END NATIVE API -->

## Observe an app

Use the app and window identities already known from the task or a previous
observation. Otherwise, list apps directly and select the requested app:

```js
var appList = await cua.list_apps();
jsRepl.write(appList.structuredContent.apps);
```

Select the intended app by its observed name, bundle ID, or launch path. If it
is not running, use `cua.launch_app(...)` with the app's observed identity.
List its windows and select the intended title:

```js
// appInfo is the selected entry from appList.structuredContent.apps.
var windows = await cua.list_windows({ pid: appInfo.pid });
jsRepl.write(windows.structuredContent.windows);
```

```js
// windowInfo is selected from windows.structuredContent.windows.
var captureArgs = { pid: appInfo.pid, window_id: windowInfo.window_id };
var state = await cua.get_window_state(captureArgs);
jsRepl.write(state.structuredContent);
for (var image of state.content ?? []) {
  if (image.type === "image") await jsRepl.emitImage(image);
}
```

Each method returns `content`, optional `structuredContent`, and optional
`isError`. If `isError` is true, read the error before deciding what to do next.
Keep native window IDs intact; do not convert them to 32-bit integers.

## Act and verify

Use an `element_token` from the latest window state when available. For pixel
actions, use coordinates from that exact window screenshot. Cua handles
backing scale; do not resize the image or add the window's screen origin.

```js
// button is the intended element from state.structuredContent.elements.
var result = await cua.click({
  pid: appInfo.pid,
  window_id: windowInfo.window_id,
  element_token: button.element_token,
  delivery_mode: "background",
});
jsRepl.write(result);
var after = await cua.get_window_state(captureArgs);
jsRepl.write(after.structuredContent);
```

Typing and key presses use the same observed window:

```js
await cua.type_text({ pid: appInfo.pid, window_id: windowInfo.window_id, text: "hello" });
await cua.press_key({ pid: appInfo.pid, window_id: windowInfo.window_id, key: "return" });
```

Confirm the intended effect from a fresh state after acting. For asynchronous
changes, poll observations with a bounded deadline without repeating the input.
Tokens expire after a new snapshot, window, or session. Refresh stale tokens.
A truncated or degraded tree may omit controls; use its screenshot and the
returned capability information when choosing another route.

Prefer background delivery. An unsupported route does not authorize an
automatic foreground retry. Windows secure/elevated desktops and Linux
compositor restrictions remain in effect. `cua.check_permissions()` reports
capabilities; macOS permission prompts belong to Waku Settings > Computer Use.

Stop, reset, and disconnect end the helper's work. An interrupted action may
have completed, so inspect fresh state before retrying it. One implicit session
is reused across calls. If using a named `session`, repeat that label on every
call that accepts it.

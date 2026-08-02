---
name: waku-computer-use
description: Control local Mac apps through Waku Computer Use when a purpose-built connector, API, or CLI is unavailable.
---

# node_repl + Sky Computer Use

Use `node_repl` (JavaScript) for all Computer Use actions. Do not call the MCP tools directly when the `sky` facade is available. The facade and its app state are persistent across calls.

Bootstrap once per fresh `node_repl` session:

```js
if (!globalThis.sky) {
  const env = globalThis.nodeRepl?.env;
  if (!env?.WAKU_COMPUTER_USE_CLIENT) {
    throw new Error("Waku Computer Use requires nodeRepl.env.WAKU_COMPUTER_USE_CLIENT");
  }
  const { setupComputerUseRuntime } = await import(env.WAKU_COMPUTER_USE_CLIENT);
  await setupComputerUseRuntime({ globals: globalThis });
}
```

Available API:

```ts
type Sky = {
  target: "mac";
  list_apps: () => Promise<Array<{
    id: string;
    displayName?: string;
    lastUsedDate?: string;
    useCount?: number;
    isRunning?: boolean;
  }>>;
  get_app_state: (args: { app: string, disableDiff?: boolean }) => Promise<{
    app: string;
    screenshot: { url: string } | null;
    text: string;
  }>;
  click: (args: { app: string, element_index?: number, x?: number, y?: number, mouse_button?: "left" | "right" | "middle" | "l" | "r" | "m", click_count?: number }) => Promise<void>;
  drag: (args: { app: string, from_x: number, from_y: number, to_x: number, to_y: number }) => Promise<void>;
  perform_secondary_action: (args: { app: string, element_index: number, action: string }) => Promise<void>;
  set_value: (args: { app: string, element_index: number, value: string }) => Promise<void>;
  select_text: (args: { app: string, element_index: number, text: string, prefix?: string, suffix?: string, selection_type?: "text" | "cursor_before" | "cursor_after" }) => Promise<void>;
  scroll: (args: { app: string, element_index: number, direction: "up" | "down" | "left" | "right" | "u" | "d" | "l" | "r", pages?: number }) => Promise<void>;
  press_key: (args: { app: string, key: string }) => Promise<void>;
  type_text: (args: { app: string, text: string }) => Promise<void>;
};
```

Start with `get_app_state` when the task names an app. It accepts a display name, full app path, process name, or bundle identifier and launches an installed app in the background when needed. Use `list_apps()` only when the app cannot be identified or a display name is ambiguous.

Call `get_app_state` again after actions and re-derive element indexes from its latest text. By default, subsequent accessibility trees may be returned as diffs; pass `disableDiff: true` when a fresh full tree is needed. Prefer element indexes over coordinates, and use screenshot coordinates when accessibility is incomplete. Screenshot URLs are `data:` URLs.

`press_key.key` uses X Window System keysym-style `+`-separated chords such as `a`, `Return`, `Tab`, `Control_L+a`, `Super_L+c`, or `KP_0`. `perform_secondary_action` only accepts an action exposed in the latest accessibility tree. Do not guess action names.

Never type credentials, approve a consequential action, or transmit sensitive data without the required user confirmation. Do not control Waku itself, password managers, security prompts, login windows, or terminal apps.

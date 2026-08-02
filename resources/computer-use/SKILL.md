---
name: waku-computer-use
description: Control local Mac apps through Waku Computer Use when a purpose-built connector, API, or CLI is unavailable.
---

# node_repl + Waku Computer Use

Use `node_repl` (JavaScript) for all Computer Use actions. Do not call the MCP tools directly from the model when the `sky` facade is available. The facade is persistent across calls and talks to Waku's bundled MCP/native-control helper.

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

The API mirrors Codex Computer Use:

```ts
sky.list_apps()
sky.get_app_state({ app, disableDiff? })
sky.click({ app, element_index?, x?, y?, mouse_button?, click_count? })
sky.drag({ app, from_x, from_y, to_x, to_y })
sky.perform_secondary_action({ app, element_index, action })
sky.set_value({ app, element_index, value })
sky.select_text({ app, element_index, text, prefix?, suffix? })
sky.scroll({ app, element_index, direction, pages? })
sky.press_key({ app, key })
sky.type_text({ app, text })
```

Call `get_app_state` before interacting and after each action batch. Prefer element indexes from the latest accessibility tree; use screenshot coordinates only when the tree is unavailable or insufficient. The helper keeps a separate software cursor, renders it in screenshots, and publishes a Waku Computer Use cursor item in the macOS menu bar.

The `app` value may be a display name, full app path, or bundle identifier. If a display-name lookup is ambiguous, retry with the exact bundle identifier from `list_apps`.

Never type credentials, approve a consequential action, or transmit sensitive data without the required user confirmation. Do not control Waku itself, password managers, security prompts, login windows, or terminal apps.

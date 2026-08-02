import assert from "node:assert/strict";
import test from "node:test";

import { createSkyFacade } from "./waku-computer-use-client.mjs";

test("sky facade matches the Codex mac surface", async () => {
  const calls = [];
  const client = {
    async call(name, arguments_) {
      calls.push([name, arguments_]);
      if (name === "list_apps") {
        return {
          structuredContent: {
            apps: [{ id: "company.thebrowser.browser", displayName: "Helium", isRunning: true }],
          },
        };
      }
      if (name === "get_app_state") {
        return {
          structuredContent: {
            text: "[0] AXWindow \"Helium\"",
            screenshot: "data:image/png;base64,c2t5",
          },
        };
      }
      return { content: [] };
    },
  };
  const sky = createSkyFacade(client);

  assert.equal(sky.target, "mac");
  assert.deepEqual(await sky.list_apps(), [
    { id: "company.thebrowser.browser", displayName: "Helium", isRunning: true },
  ]);
  assert.deepEqual(
    await sky.get_app_state({ app: "company.thebrowser.browser", disableDiff: true }),
    {
      app: "company.thebrowser.browser",
      text: "[0] AXWindow \"Helium\"",
      screenshot: { url: "data:image/png;base64,c2t5" },
    },
  );
  await sky.select_text({
    app: "company.thebrowser.browser",
    element_index: 12,
    text: "browser",
    selection_type: "cursor_after",
  });

  assert.deepEqual(calls, [
    ["list_apps", {}],
    ["get_app_state", { app: "company.thebrowser.browser", disableDiff: true }],
    ["select_text", {
      app: "company.thebrowser.browser",
      element_index: 12,
      text: "browser",
      selection_type: "cursor_after",
    }],
  ]);
});

test("get_app_state preserves MCP image data as a data URL", async () => {
  const sky = createSkyFacade({
    async call() {
      return {
        content: [
          { type: "text", text: "tree" },
          { type: "image", data: "c2t5", mimeType: "image/png" },
        ],
      };
    },
  });

  assert.deepEqual(await sky.get_app_state({ app: "Notes" }), {
    app: "Notes",
    text: "tree",
    screenshot: { url: "data:image/png;base64,c2t5" },
  });
});

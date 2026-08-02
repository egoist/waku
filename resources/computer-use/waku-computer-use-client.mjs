import { spawn } from "node:child_process";
import { promises as fs } from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const RUNTIME_KEY = Symbol.for("waku.computer-use.runtime");

/**
 * Install the Waku-owned `sky` facade in a persistent node_repl session.
 * The facade deliberately mirrors the Codex Computer Use surface while the
 * bundled Swift helper remains the MCP/native-control boundary.
 */
export async function setupComputerUseRuntime({ globals = globalThis } = {}) {
  const installed = Reflect.get(globalThis, RUNTIME_KEY);
  if (installed) {
    Reflect.set(globalThis, "sky", installed);
    Reflect.set(globals, "sky", installed);
    return installed;
  }

  const env = globalThis.nodeRepl?.env ?? {};
  const client = new MCPClient(
    env.WAKU_COMPUTER_USE_SERVER || await bundledHelperCommand(),
    env.WAKU_COMPUTER_USE_ARGS ? JSON.parse(env.WAKU_COMPUTER_USE_ARGS) : ["mcp"],
  );
  await client.initialize();

  const sky = Object.freeze({
    target: "mac",
    list_apps: async () => parseJSONText(await client.call("list_apps", {})),
    get_app_state: async ({ app, disableDiff: _disableDiff } = {}) => {
      const result = await client.call("get_app_state", { app });
      const text = textContent(result);
      const image = imageContent(result);
      return {
        app,
        text,
        screenshot: image ? { url: await writeScreenshot(image) } : null,
      };
    },
    click: async (args) => { await client.call("click", args); },
    drag: async (args) => { await client.call("drag", args); },
    perform_secondary_action: async (args) => { await client.call("perform_secondary_action", args); },
    set_value: async (args) => { await client.call("set_value", args); },
    select_text: async (args) => { await client.call("select_text", args); },
    scroll: async (args) => { await client.call("scroll", args); },
    press_key: async (args) => { await client.call("press_key", args); },
    type_text: async (args) => { await client.call("type_text", args); },
  });

  Reflect.set(globalThis, RUNTIME_KEY, sky);
  Reflect.set(globalThis, "sky", sky);
  Reflect.set(globals, "sky", sky);
  return sky;
}

async function bundledHelperCommand() {
  const resources = path.dirname(fileURLToPath(import.meta.url));
  const helpers = path.join(resources, "..", "Helpers");
  const entries = await fs.readdir(helpers, { withFileTypes: true });
  const candidates = [];
  for (const entry of entries) {
    if (!entry.isDirectory() || !entry.name.endsWith(" Computer Use.app")) continue;
    const executable = entry.name.slice(0, -".app".length);
    candidates.push(
      path.join(os.homedir(), "Library", "Application Support", "Waku", "Computer Use", entry.name, "Contents", "MacOS", executable),
      path.join(helpers, entry.name, "Contents", "MacOS", executable),
    );
  }
  for (const candidate of candidates) {
    try {
      await fs.access(candidate);
      return candidate;
    } catch {
      // Try the next bundled profile.
    }
  }
  throw new Error("Waku Computer Use helper was not found next to the client");
}

class MCPClient {
  constructor(command, args) {
    if (!command) throw new Error("Waku Computer Use requires WAKU_COMPUTER_USE_SERVER");
    this.child = spawn(command, args, { stdio: ["pipe", "pipe", "pipe"] });
    this.nextId = 1;
    this.pending = new Map();
    this.buffer = "";
    this.child.stdout.setEncoding("utf8");
    this.child.stdout.on("data", (chunk) => this.read(chunk));
    this.child.on("error", (error) => this.rejectAll(error));
    this.child.on("exit", (code) => this.rejectAll(new Error(`Waku Computer Use exited (${code ?? "unknown"})`)));
  }

  initialize() {
    return this.request("initialize", {
      protocolVersion: "2025-06-18",
      capabilities: {},
      clientInfo: { name: "Waku node_repl", version: "1.0.0" },
    }).then(() => this.notify("notifications/initialized", {}));
  }

  call(name, arguments_) {
    return this.request("tools/call", { name, arguments: arguments_ });
  }

  notify(method, params) {
    this.child.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", method, params })}\n`);
  }

  request(method, params) {
    const id = this.nextId++;
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
      this.child.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", id, method, params })}\n`);
    }).then((response) => {
      if (response.error) throw new Error(response.error.message || "Computer Use MCP request failed");
      if (response.result?.isError) throw new Error(textContent(response.result) || "Computer Use action failed");
      return response.result;
    });
  }

  read(chunk) {
    this.buffer += chunk;
    let newline;
    while ((newline = this.buffer.indexOf("\n")) >= 0) {
      const line = this.buffer.slice(0, newline);
      this.buffer = this.buffer.slice(newline + 1);
      if (!line.trim()) continue;
      let message;
      try { message = JSON.parse(line); } catch { continue; }
      const request = this.pending.get(message.id);
      if (!request) continue;
      this.pending.delete(message.id);
      request.resolve(message);
    }
  }

  rejectAll(error) {
    for (const { reject } of this.pending.values()) reject(error);
    this.pending.clear();
  }
}

function textContent(result) {
  return (result?.content || [])
    .filter((item) => item.type === "text")
    .map((item) => item.text)
    .join("\n\n");
}

function imageContent(result) {
  return (result?.content || []).find((item) => item.type === "image" && item.data);
}

function parseJSONText(result) {
  const text = textContent(result);
  try { return JSON.parse(text); } catch { return []; }
}

async function writeScreenshot(image) {
  const directory = await fs.mkdtemp(path.join(os.tmpdir(), "waku-computer-use-"));
  const filename = path.join(directory, "screenshot.png");
  await fs.writeFile(filename, Buffer.from(image.data, "base64"));
  return pathToFileURL(filename).href;
}

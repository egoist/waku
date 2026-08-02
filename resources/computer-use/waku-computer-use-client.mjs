import { spawn } from "node:child_process";
import { promises as fs } from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const COMPUTER_USE_RUNTIME_KEY = Symbol.for("openai.computer-use.runtime");
const CHROME_COMPUTER_USE_META_KEY = "codex/computerUseChrome";
const MAC_CHROME_APP_PATH_PATTERN = /(?:^|[\\/])Google Chrome\.app(?:[\\/]|$)/i;

/**
 * Install the Codex-compatible `sky` surface in a persistent node_repl
 * session while the bundled Swift helper remains the native-control boundary.
 */
export async function setupComputerUseRuntime({ globals = globalThis } = {}) {
  const installed = Reflect.get(globalThis, COMPUTER_USE_RUNTIME_KEY);
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

  const sky = createSkyFacade(client);
  installChromeComputerUseMetadata(sky);
  Object.freeze(sky);

  Reflect.set(globalThis, COMPUTER_USE_RUNTIME_KEY, sky);
  Reflect.set(globalThis, "sky", sky);
  Reflect.set(globals, "sky", sky);
  return sky;
}

export function createSkyFacade(client) {
  return {
    target: "mac",
    list_apps: async () => parseJSONText(await client.call("list_apps", {})),
    get_app_state: async ({ app, disableDiff } = {}) => {
      const result = await client.call("get_app_state", withoutUndefined({ app, disableDiff }));
      const structured = result?.structuredContent;
      const text = typeof structured?.text === "string" ? structured.text : textContent(result);
      const screenshotUrl = typeof structured?.screenshot === "string"
        ? structured.screenshot
        : imageDataUrl(imageContent(result));
      return {
        app,
        text,
        screenshot: screenshotUrl ? { url: screenshotUrl } : null,
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
  };
}

async function bundledHelperCommand() {
  const resources = path.dirname(fileURLToPath(import.meta.url));
  const helpers = path.join(resources, "..", "Helpers");
  const entries = await fs.readdir(helpers, { withFileTypes: true });
  for (const entry of entries) {
    if (!entry.isDirectory() || !entry.name.endsWith(" Computer Use.app")) continue;
    const executable = entry.name.slice(0, -".app".length);
    const candidate = path.join(
      os.homedir(),
      "Library",
      "Application Support",
      "Waku",
      "Computer Use",
      entry.name,
      "Contents",
      "MacOS",
      executable,
    );
    try {
      await fs.access(candidate);
      return candidate;
    } catch {
      // The host installs the packaged helper before exposing this client.
    }
  }
  throw new Error("Waku Computer Use service is not installed; restart the current Waku session");
}

class MCPClient {
  constructor(command, args) {
    if (!command) throw new Error("Waku Computer Use requires WAKU_COMPUTER_USE_SERVER");
    this.command = command;
    this.args = args;
    this.child = null;
    this.initialized = false;
    this.nextId = 1;
    this.pending = new Map();
    this.buffer = "";
  }

  async initialize() {
    this.ensureChild();
    await this.request("initialize", {
      protocolVersion: "2025-06-18",
      capabilities: {},
      clientInfo: { name: "Waku node_repl", version: "1.0.0" },
    });
    this.notify("notifications/initialized", {});
    this.initialized = true;
  }

  async call(name, arguments_) {
    if (!this.initialized || !this.isChildRunning()) await this.initialize();
    return this.request("tools/call", { name, arguments: arguments_ });
  }

  ensureChild() {
    if (this.isChildRunning()) return;
    const child = spawn(this.command, this.args, { stdio: ["pipe", "pipe", "pipe"] });
    this.child = child;
    this.initialized = false;
    this.buffer = "";
    child.stdout.setEncoding("utf8");
    child.stdout.on("data", (chunk) => this.read(chunk));
    child.on("error", (error) => {
      if (this.child === child) this.child = null;
      this.initialized = false;
      this.rejectAll(error);
    });
    child.on("exit", (code) => {
      if (this.child === child) {
        this.child = null;
        this.initialized = false;
      }
      this.rejectAll(new Error(`Waku Computer Use exited (${code ?? "unknown"})`));
    });
  }

  isChildRunning() {
    return this.child !== null && this.child.exitCode === null && !this.child.killed;
  }

  notify(method, params) {
    if (!this.isChildRunning()) throw new Error("Waku Computer Use MCP bridge is not running");
    this.child.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", method, params })}\n`);
  }

  request(method, params) {
    if (!this.isChildRunning()) return Promise.reject(new Error("Waku Computer Use MCP bridge is not running"));
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

  close() {
    const child = this.child;
    this.child = null;
    this.initialized = false;
    if (child && !child.killed) child.stdin.end();
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
  if (Array.isArray(result?.structuredContent?.apps)) return result.structuredContent.apps;
  const text = textContent(result);
  try { return JSON.parse(text); } catch { return []; }
}

function imageDataUrl(image) {
  if (!image?.data) return null;
  const mimeType = image.mimeType || image.mime_type || "image/png";
  return `data:${mimeType};base64,${image.data}`;
}

function withoutUndefined(value) {
  return Object.fromEntries(Object.entries(value).filter(([, item]) => item !== undefined));
}

function installChromeComputerUseMetadata(sky) {
  for (const [property, value] of Object.entries(sky)) {
    if (property === "list_apps" || typeof value !== "function") continue;
    Reflect.set(sky, property, (...args) => {
      if (isChromeComputerUseInput(args[0])) {
        globalThis.nodeRepl?.setResponseMeta?.({ [CHROME_COMPUTER_USE_META_KEY]: true });
      }
      return Reflect.apply(value, sky, args);
    });
  }
}

function isChromeComputerUseInput(input) {
  if (typeof input !== "object" || input === null) return false;
  const descriptor = Object.getOwnPropertyDescriptor(input, "app");
  if (!descriptor || !("value" in descriptor) || typeof descriptor.value !== "string") {
    return false;
  }
  const app = descriptor.value.trim();
  return ["chrome", "google chrome", "com.google.chrome"].includes(app.toLowerCase())
    || MAC_CHROME_APP_PATH_PATTERN.test(app);
}

import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process";
import { createInterface } from "node:readline";
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { Type } from "typebox";

type JsonObject = Record<string, unknown>;

const jsToolDescription =
  "Run JavaScript in Waku's persistent QuickJS kernel for Computer Use. Initialize `sky` lazily with `await setupComputerUseRuntime({ globals: globalThis })`. Calls time out after 30000 ms (30 seconds) unless `timeout_ms` is provided. Use `nodeRepl.write(...)` for text and `await nodeRepl.emitImage(...)` for images. Bindings and scheduled timers persist until the JavaScript kernel is reset.";

class WakuMcpClient {
  private child: ChildProcessWithoutNullStreams | undefined;
  private starting: Promise<void> | undefined;
  private nextId = 0;
  private pending = new Map<
    number,
    { resolve: (value: unknown) => void; reject: (error: Error) => void }
  >();

  private async ensureStarted(): Promise<void> {
    if (this.child) return;
    if (this.starting) return this.starting;
    this.starting = this.start();
    try {
      await this.starting;
    } finally {
      this.starting = undefined;
    }
  }

  private async start(): Promise<void> {
    const executable = process.env.WAKU_JS_REPL_SERVER;
    if (!executable) {
      throw new Error("WAKU_JS_REPL_SERVER is not configured");
    }
    const child = spawn(executable, [], {
      env: process.env,
      stdio: ["pipe", "pipe", "pipe"],
    });
    this.child = child;
    const lines = createInterface({ input: child.stdout });
    lines.on("line", (line) => this.handleLine(line));
    child.stderr.on("data", (chunk) => {
      const message = String(chunk).trim();
      if (message) console.error(`Waku JavaScript REPL: ${message}`);
    });
    child.once("error", (error) => this.handleExit(error));
    child.once("exit", (code, signal) => {
      this.handleExit(
        new Error(
          `Waku JavaScript REPL exited${code === null ? "" : ` with ${code}`}${
            signal ? ` (${signal})` : ""
          }`,
        ),
      );
    });
    await this.requestWithoutStart("initialize", {
      protocolVersion: "2025-06-18",
      capabilities: {},
      clientInfo: { name: "waku-pi", version: "1" },
    });
    this.notify("notifications/initialized", {});
  }

  private handleLine(line: string): void {
    let message: JsonObject;
    try {
      message = JSON.parse(line) as JsonObject;
    } catch {
      return;
    }
    const id = typeof message.id === "number" ? message.id : undefined;
    if (id === undefined) return;
    const pending = this.pending.get(id);
    if (!pending) return;
    this.pending.delete(id);
    if (message.error && typeof message.error === "object") {
      const detail = (message.error as JsonObject).message;
      pending.reject(
        new Error(typeof detail === "string" ? detail : "MCP request failed"),
      );
    } else {
      pending.resolve(message.result);
    }
  }

  private handleExit(error: Error): void {
    this.child = undefined;
    for (const pending of this.pending.values()) pending.reject(error);
    this.pending.clear();
  }

  private notify(method: string, params: JsonObject): void {
    this.child?.stdin.write(
      `${JSON.stringify({ jsonrpc: "2.0", method, params })}\n`,
    );
  }

  private requestWithoutStart(method: string, params: JsonObject): Promise<unknown> {
    const child = this.child;
    if (!child) return Promise.reject(new Error("Waku JavaScript REPL is not running"));
    const id = ++this.nextId;
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
      child.stdin.write(
        `${JSON.stringify({ jsonrpc: "2.0", id, method, params })}\n`,
        (error) => {
          if (!error) return;
          this.pending.delete(id);
          reject(error);
        },
      );
    });
  }

  async call(name: string, args: JsonObject): Promise<JsonObject> {
    await this.ensureStarted();
    return (await this.requestWithoutStart("tools/call", {
      name,
      arguments: args,
    })) as JsonObject;
  }

  async close(): Promise<void> {
    const child = this.child;
    if (!child) return;
    try {
      await this.requestWithoutStart("shutdown", {});
    } catch {
      // The child may already be closing with Pi.
    }
    child.stdin.end();
    if (this.child === child) child.kill("SIGTERM");
  }
}

function toolResult(result: JsonObject) {
  const content = Array.isArray(result.content) ? result.content : [];
  if (result.isError === true) {
    const message = content
      .filter(
        (item): item is { type: "text"; text: string } =>
          !!item &&
          typeof item === "object" &&
          (item as JsonObject).type === "text" &&
          typeof (item as JsonObject).text === "string",
      )
      .map((item) => item.text)
      .join("\n");
    throw new Error(message || "Waku JavaScript execution failed");
  }
  return { content, details: result._meta ?? {} };
}

export default function wakuComputerUse(pi: ExtensionAPI) {
  const client = new WakuMcpClient();

  pi.registerTool({
    name: "js",
    label: "JavaScript",
    description: jsToolDescription,
    parameters: Type.Object(
      {
        code: Type.String({
          description:
            "JavaScript source to execute in the persistent QuickJS kernel. The code runs with top-level await and can use `sky` and the `nodeRepl` helpers.",
        }),
        timeout_ms: Type.Optional(
          Type.Integer({
            minimum: 1,
            description:
              "Optional execution timeout in milliseconds. Defaults to 30000 (30 seconds) when omitted.",
          }),
        ),
        title: Type.Optional(
          Type.String({
            minLength: 1,
            maxLength: 80,
            description:
              "Short user-facing description of what this code block is doing.",
          }),
        ),
      },
      { additionalProperties: false },
    ),
    executionMode: "sequential",
    async execute(_id, params) {
      return toolResult(await client.call("js", params));
    },
  });

  pi.registerTool({
    name: "js_reset",
    label: "Reset JavaScript",
    description:
      "Reset the persistent JavaScript kernel and clear all bindings created by prior `js` calls. The `nodeRepl` helpers and lazy `setupComputerUseRuntime(...)` entrypoint are installed again automatically; `sky` remains unset until setup is called.",
    parameters: Type.Object({}, { additionalProperties: false }),
    executionMode: "sequential",
    async execute() {
      return toolResult(await client.call("js_reset", {}));
    },
  });

  pi.on("session_shutdown", async () => {
    await client.close();
  });
}

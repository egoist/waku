// Protocol/SDK smoke test. No app discovery, screenshots, input, or TCC prompts.
// Usage: bun scripts/test-computer-use.ts <waku_js_repl> <computer-use-helper>
import assert from "node:assert/strict";
import { execFileSync, spawn } from "node:child_process";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { resolve, join } from "node:path";
import { createInterface } from "node:readline";

const suffix = process.platform === "win32" ? ".exe" : "";
const expectCursor = process.argv.includes("--expect-cursor");
const [
  repl = `target/debug/waku_js_repl${suffix}`,
  helper = `target/debug/waku_computer_use${suffix}`,
] = process.argv.slice(2).filter((argument) => argument !== "--expect-cursor");
assert(repl && helper, "Pass the REPL executable and native helper executable");
const nativeTools: string[] = JSON.parse(
  execFileSync(resolve(helper), ["list-tools"], {
    encoding: "utf8",
    windowsHide: true,
    maxBuffer: 8 * 1024 * 1024,
  }),
)
  .tools.map((tool: { name: string }) => tool.name)
  .filter((name: string) => name !== "bring_to_front");
const directory = await mkdtemp(join(tmpdir(), "waku-cua-test-"));
const child = spawn(resolve(repl), [], {
  env: {
    ...process.env,
    WAKU_COMPUTER_USE_SERVER: resolve(helper),
    WAKU_COMPUTER_USE_PROCESS_DIRECTORY: directory,
  },
  stdio: ["pipe", "pipe", "pipe"],
  windowsHide: true,
});
let nextId = 0;
const pending = new Map<
  number,
  { resolve(value: any): void; reject(error: Error): void }
>();
createInterface({ input: child.stdout }).on("line", (line) => {
  const message = JSON.parse(line);
  const request = pending.get(message.id);
  if (!request) return;
  pending.delete(message.id);
  if (message.error) request.reject(new Error(message.error.message));
  else request.resolve(message.result);
});
child.stderr.on("data", (chunk) => process.stderr.write(chunk));
child.on("error", (error) => {
  for (const request of pending.values()) request.reject(error);
});
child.on("exit", (code) => {
  for (const request of pending.values())
    request.reject(new Error(`REPL exited: ${code}`));
});
function request(method: string, params: object): Promise<any> {
  const id = ++nextId;
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      pending.delete(id);
      reject(new Error(`Timed out: ${method}`));
    }, 30_000);
    pending.set(id, {
      resolve: (value) => {
        clearTimeout(timer);
        resolve(value);
      },
      reject: (error) => {
        clearTimeout(timer);
        reject(error);
      },
    });
    child.stdin.write(
      JSON.stringify({ jsonrpc: "2.0", id, method, params }) + "\n",
    );
  });
}
async function js(code: string): Promise<any> {
  const result = await request("tools/call", {
    name: "js",
    arguments: { code },
  });
  assert.notEqual(result.isError, true, JSON.stringify(result));
  return result;
}
try {
  await request("initialize", {
    protocolVersion: "2025-06-18",
    capabilities: {},
    clientInfo: { name: "waku-cua-test", version: "1" },
  });
  const initial = await js("jsRepl.write(typeof cua)");
  assert.equal(initial.content[0].text, "undefined");
  const result = await js(`
    await setupComputerUseRuntime({ globals: globalThis });
    var tools = ${JSON.stringify(nativeTools)};
    var directTools = tools.every(name =>
      Object.hasOwn(cua, name) && typeof cua[name] === "function");
    var hasDiscoveryApi = "list_tools" in cua || "call_tool" in cua;
    var config = await cua.get_config();
    // Missing required IDs are rejected before any window capture.
    var refused = await cua.get_window_state({});
    var hasBringToFront = "bring_to_front" in cua;
    var cursor = null;
    if (${expectCursor}) {
      await cua.start_session({ session: "waku-cursor-smoke" });
      cursor = await cua.get_agent_cursor_state({ session: "waku-cursor-smoke" });
      await cua.end_session({ session: "waku-cursor-smoke" });
    }
    var savedGetConfig = cua.get_config;
    var nextConfig = await savedGetConfig({});
    var sameRuntime = await setupComputerUseRuntime({ globals: globalThis }) === cua;
    var invalidArguments = false;
    try { await cua.get_config([]); } catch (error) { invalidArguments = error instanceof TypeError; }
    jsRepl.write(JSON.stringify({ platform: cua.platform, tools,
      directTools, hasDiscoveryApi, sameRuntime, frozen: Object.isFrozen(cua), invalidArguments, config, refused, hasBringToFront, cursor, nextConfig }));
  `);
  const output = JSON.parse(result.content[0].text);
  assert(output.tools.includes("list_apps"));
  assert(output.tools.includes("get_window_state"));
  assert(output.tools.includes("click"));
  assert.equal(output.directTools, true);
  assert.equal(output.hasDiscoveryApi, false);
  assert.equal(output.sameRuntime, true);
  assert.equal(output.frozen, true);
  assert.equal(output.invalidArguments, true);
  assert.notEqual(output.config.isError, true);
  assert.equal(output.refused.isError, true);
  assert(Array.isArray(output.refused.content));
  assert.equal(output.hasBringToFront, false);
  if (expectCursor) {
    assert.notEqual(output.cursor.isError, true, JSON.stringify(output.cursor));
    assert.equal(output.cursor.structuredContent.enabled, true);
  }
  assert.notEqual(output.nextConfig.isError, true);
  assert(output.nextConfig.structuredContent);
  const image = await js(
    'await jsRepl.emitImage({ type: "image", mimeType: "image/png", data: "cG5n" })',
  );
  assert(
    image.content.some(
      (item: any) => item.type === "image" && item.data === "cG5n",
    ),
  );
  await request("tools/call", { name: "js_reset", arguments: {} });
  assert.equal(
    (await js("jsRepl.write(typeof cua)")).content[0].text,
    "undefined",
  );
  const resetConfig = await js(
    "await setupComputerUseRuntime({ globals: globalThis }); jsRepl.write(JSON.stringify(await cua.get_config()))",
  );
  assert.notEqual(JSON.parse(resetConfig.content[0].text).isError, true);
  console.log(
    `Cua SDK smoke passed (${output.platform}, ${output.tools.length} direct native methods; refusal, images, reset and reconnect${expectCursor ? ", native cursor enabled" : ""}).`,
  );
} finally {
  child.stdin.end();
  await Promise.race([
    new Promise((resolve) => child.once("exit", resolve)),
    new Promise((resolve) => setTimeout(resolve, 2_000)),
  ]);
  if (child.exitCode === null) child.kill();
  await rm(directory, { recursive: true, force: true });
}

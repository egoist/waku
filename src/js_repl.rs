use std::fs;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context as _, anyhow, bail};
use base64::Engine as _;
use parking_lot::Mutex;
use rquickjs::context::EvalOptions;
use rquickjs::{Context, Ctx, Exception, Function, Promise, Runtime, Value};
use serde_json::{Map, Value as JsonValue, json};

const MCP_PROTOCOL_VERSION: &str = "2025-06-18";
const DEFAULT_EXECUTION_TIMEOUT_MS: u64 = 30_000;
const MAX_REQUEST_BYTES: usize = 16 * 1024 * 1024;

const SERVER_INSTRUCTIONS: &str = "Use `js` to run JavaScript in the persistent QuickJS kernel. When a skill or prompt says to use `node_repl`, call this server's `js` execution tool. Calls default to a 30000 ms (30 seconds) timeout when `timeout_ms` is omitted. Waku Computer Use is initialized lazily with `await setupComputerUseRuntime({ globals: globalThis })`; that installs `sky` without importing a module. The runtime also exposes `nodeRepl.cwd`, `nodeRepl.homeDir`, `nodeRepl.tmpDir`, `nodeRepl.requestMeta`, `nodeRepl.setResponseMeta(...)`, `nodeRepl.write(...)`, and `await nodeRepl.emitImage(...)`. Top-level bindings persist across `js` calls until `js_reset`; do not redeclare existing `const` or `let` names. Reuse existing bindings, use top-level `var` for reusable state that may be assigned again, or choose a fresh descriptive name.";

const KERNEL_BOOTSTRAP: &str = r#"
(() => {
  const format = (value, seen = new Set()) => {
    if (typeof value === "string") return value;
    if (typeof value === "bigint") return `${value}n`;
    if (typeof value === "undefined") return "undefined";
    if (typeof value === "function") return `[Function${value.name ? `: ${value.name}` : ""}]`;
    if (value === null || typeof value !== "object") return String(value);
    if (seen.has(value)) return "[Circular]";
    seen.add(value);
    try {
      if (value instanceof Error) return value.stack || `${value.name}: ${value.message}`;
      if (Array.isArray(value)) return `[ ${value.map((item) => format(item, seen)).join(", ")} ]`;
      const entries = Object.entries(value).map(([key, item]) => `${key}: ${format(item, seen)}`);
      return `{ ${entries.join(", ")} }`;
    } finally {
      seen.delete(value);
    }
  };

  const nativeSkyCall = globalThis.__wakuSkyCall;
  const nativeWrite = globalThis.__wakuWrite;
  const nativeEmitImage = globalThis.__wakuEmitImage;
  const nativeSetResponseMeta = globalThis.__wakuSetResponseMeta;
  const cwd = globalThis.__wakuCwd;
  const homeDir = globalThis.__wakuHomeDir;
  const tmpDir = globalThis.__wakuTmpDir;
  delete globalThis.__wakuSkyCall;
  delete globalThis.__wakuWrite;
  delete globalThis.__wakuEmitImage;
  delete globalThis.__wakuSetResponseMeta;
  delete globalThis.__wakuCwd;
  delete globalThis.__wakuHomeDir;
  delete globalThis.__wakuTmpDir;

  const write = (value, newline = false) => nativeWrite(format(value), newline);
  globalThis.console = Object.freeze({
    log: (...values) => nativeWrite(values.map((value) => format(value)).join(" "), true),
    info: (...values) => nativeWrite(values.map((value) => format(value)).join(" "), true),
    warn: (...values) => nativeWrite(values.map((value) => format(value)).join(" "), true),
    error: (...values) => nativeWrite(values.map((value) => format(value)).join(" "), true),
  });

  const chromeComputerUseMetaKey = "codex/computerUseChrome";
  const macChromeAppPathPattern = /(?:^|[\\/])Google Chrome\.app(?:[\\/]|$)/i;
  const markChromeComputerUse = (arguments_) => {
    if (typeof arguments_ !== "object" || arguments_ === null) return;
    const descriptor = Object.getOwnPropertyDescriptor(arguments_, "app");
    if (!descriptor || !("value" in descriptor) || typeof descriptor.value !== "string") return;
    const app = descriptor.value.trim();
    if (
      ["chrome", "google chrome", "com.google.chrome"].includes(app.toLowerCase()) ||
      macChromeAppPathPattern.test(app)
    ) {
      nativeSetResponseMeta(JSON.stringify({ [chromeComputerUseMetaKey]: true }));
    }
  };
  const nativeCall = (name, arguments_ = {}) => {
    if (name !== "list_apps") markChromeComputerUse(arguments_);
    const envelope = JSON.parse(nativeSkyCall(name, JSON.stringify(arguments_ ?? {})));
    if (!envelope.ok) throw new Error(envelope.error || `Computer Use ${name} failed`);
    return envelope.value;
  };
  const computerUseRuntimeKey = Symbol.for("openai.computer-use.runtime");
  globalThis.setupComputerUseRuntime = async ({ globals = globalThis } = {}) => {
    let sky = globalThis[computerUseRuntimeKey];
    if (!sky) {
      sky = Object.freeze({
        target: "mac",
        list_apps: async () => nativeCall("list_apps"),
        get_app_state: async (arguments_ = {}) => nativeCall("get_app_state", arguments_),
        click: async (arguments_) => { nativeCall("click", arguments_); },
        drag: async (arguments_) => { nativeCall("drag", arguments_); },
        perform_secondary_action: async (arguments_) => { nativeCall("perform_secondary_action", arguments_); },
        set_value: async (arguments_) => { nativeCall("set_value", arguments_); },
        select_text: async (arguments_) => { nativeCall("select_text", arguments_); },
        scroll: async (arguments_) => { nativeCall("scroll", arguments_); },
        press_key: async (arguments_) => { nativeCall("press_key", arguments_); },
        type_text: async (arguments_) => { nativeCall("type_text", arguments_); },
      });
      Object.defineProperty(globalThis, computerUseRuntimeKey, { value: sky });
    }
    Reflect.set(globalThis, "sky", sky);
    Reflect.set(globals, "sky", sky);
    return sky;
  };

  globalThis.nodeRepl = {
    cwd,
    homeDir,
    tmpDir,
    requestMeta: {},
    write: (value) => write(value, false),
    setResponseMeta: (meta) => {
      const envelope = JSON.parse(nativeSetResponseMeta(JSON.stringify(meta ?? {})));
      if (!envelope.ok) throw new TypeError(envelope.error || "response metadata must be an object");
    },
    emitImage: async (imageLike) => {
      const candidate = typeof imageLike === "string" ? imageLike : imageLike?.url;
      if (typeof candidate !== "string") {
        throw new TypeError("emitImage expects a data URL, file URL, or { url } object");
      }
      const envelope = JSON.parse(nativeEmitImage(candidate));
      if (!envelope.ok) throw new Error(envelope.error || "could not emit image");
    },
  };
})();
"#;

#[derive(Default)]
struct CallOutput {
    text: String,
    images: Vec<EmittedImage>,
    response_meta: Map<String, JsonValue>,
    request_meta: JsonValue,
}

struct EmittedImage {
    data: String,
    mime_type: String,
}

struct Kernel {
    _runtime: Runtime,
    context: Context,
}

pub fn serve_stdio() -> anyhow::Result<()> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    serve(BufReader::new(stdin.lock()), BufWriter::new(stdout.lock()))
}

fn serve<R: BufRead, W: Write>(mut input: R, mut output: W) -> anyhow::Result<()> {
    let mut repl = JavaScriptRepl::new()?;
    let mut line = String::new();
    loop {
        line.clear();
        let bytes = input.read_line(&mut line)?;
        if bytes == 0 {
            break;
        }
        if line.len() > MAX_REQUEST_BYTES {
            write_message(
                &mut output,
                &json_rpc_error(JsonValue::Null, -32600, "MCP request is too large"),
            )?;
            continue;
        }
        let message: JsonValue = match serde_json::from_str(line.trim()) {
            Ok(message) => message,
            Err(error) => {
                write_message(
                    &mut output,
                    &json_rpc_error(JsonValue::Null, -32700, &format!("invalid JSON: {error}")),
                )?;
                continue;
            }
        };
        let Some(method) = message.get("method").and_then(JsonValue::as_str) else {
            if message.get("id").is_some() {
                write_message(
                    &mut output,
                    &json_rpc_error(
                        message.get("id").cloned().unwrap_or(JsonValue::Null),
                        -32600,
                        "JSON-RPC method is required",
                    ),
                )?;
            }
            continue;
        };
        if message.get("id").is_none() {
            if method == "exit" {
                break;
            }
            continue;
        }
        let id = message.get("id").cloned().unwrap_or(JsonValue::Null);
        let params = message.get("params").cloned().unwrap_or_else(|| json!({}));
        let response = match method {
            "initialize" => json_rpc_result(id, initialize_result()),
            "ping" => json_rpc_result(id, json!({})),
            "tools/list" => json_rpc_result(id, json!({"tools": tool_definitions()})),
            "tools/call" => match call_tool(&mut repl, &params) {
                Ok(result) => json_rpc_result(id, result),
                Err(error) => json_rpc_error(id, -32602, &error.to_string()),
            },
            "shutdown" => json_rpc_result(id, JsonValue::Null),
            _ => json_rpc_error(id, -32601, &format!("unsupported MCP method: {method}")),
        };
        write_message(&mut output, &response)?;
        if method == "shutdown" {
            break;
        }
    }
    Ok(())
}

fn initialize_result() -> JsonValue {
    json!({
        "protocolVersion": MCP_PROTOCOL_VERSION,
        "capabilities": {"tools": {"listChanged": false}},
        "serverInfo": {
            "name": "waku_js_repl",
            "version": env!("CARGO_PKG_VERSION")
        },
        "instructions": SERVER_INSTRUCTIONS
    })
}

fn tool_definitions() -> Vec<JsonValue> {
    vec![
        json!({
            "name": "js",
            "description": "Run JavaScript in a persistent QuickJS kernel with top-level await. This is the JavaScript execution tool for Waku Computer Use; initialize `sky` lazily with `await setupComputerUseRuntime({ globals: globalThis })`. If `timeout_ms` is omitted, execution times out after 30000 ms (30 seconds). Use `nodeRepl.write(...)` for text and `await nodeRepl.emitImage(...)` for images. Top-level bindings persist across calls until `js_reset`.",
            "inputSchema": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "code": {
                        "type": "string",
                        "description": "JavaScript source to execute in the persistent QuickJS kernel. The code runs with top-level await and can use `sky` and the `nodeRepl` helpers."
                    },
                    "timeout_ms": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "Optional execution timeout in milliseconds. Defaults to 30000 (30 seconds) when omitted."
                    },
                    "title": {
                        "type": "string",
                        "minLength": 1,
                        "maxLength": 80,
                        "description": "Short user-facing description of what this code block is doing."
                    }
                },
                "required": ["code"]
            }
        }),
        json!({
            "name": "js_reset",
            "description": "Reset the persistent JavaScript kernel and clear all bindings created by prior `js` calls. The `nodeRepl` helpers and lazy `setupComputerUseRuntime(...)` entrypoint are installed again automatically; `sky` remains unset until setup is called.",
            "inputSchema": {
                "type": "object",
                "additionalProperties": false,
                "properties": {}
            },
            "annotations": {
                "readOnlyHint": true,
                "destructiveHint": false,
                "openWorldHint": false
            }
        }),
    ]
}

fn call_tool(repl: &mut JavaScriptRepl, params: &JsonValue) -> anyhow::Result<JsonValue> {
    let object = params
        .as_object()
        .ok_or_else(|| anyhow!("tools/call params must be an object"))?;
    let name = object
        .get("name")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| anyhow!("tools/call requires a tool name"))?;
    let arguments = object
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let arguments = arguments
        .as_object()
        .ok_or_else(|| anyhow!("tool arguments must be an object"))?;
    match name {
        "js" => {
            let code = arguments
                .get("code")
                .and_then(JsonValue::as_str)
                .ok_or_else(|| anyhow!("js requires string `code`"))?;
            let timeout_ms = arguments
                .get("timeout_ms")
                .map(|value| {
                    value
                        .as_u64()
                        .filter(|value| *value > 0)
                        .ok_or_else(|| anyhow!("timeout_ms must be a positive integer"))
                })
                .transpose()?
                .unwrap_or(DEFAULT_EXECUTION_TIMEOUT_MS);
            let timeout = Duration::from_millis(timeout_ms);
            if Instant::now().checked_add(timeout).is_none() {
                bail!("timeout_ms is too large");
            }
            if let Some(title) = arguments.get("title") {
                let title = title
                    .as_str()
                    .filter(|title| !title.is_empty() && title.chars().count() <= 80)
                    .ok_or_else(|| anyhow!("title must contain between 1 and 80 characters"))?;
                let _ = title;
            }
            let request_meta = params.get("_meta").cloned().unwrap_or_else(|| json!({}));
            Ok(repl.execute(code, timeout, request_meta))
        }
        "js_reset" => {
            if !arguments.is_empty() {
                bail!("js_reset does not accept arguments");
            }
            repl.reset()?;
            Ok(tool_result(
                "js kernel reset",
                false,
                Vec::new(),
                Map::new(),
            ))
        }
        _ => bail!("unknown tool: {name}"),
    }
}

struct JavaScriptRepl {
    kernel: Kernel,
    bridge: Arc<Mutex<NativeComputerUseClient>>,
    call_output: Arc<Mutex<Option<CallOutput>>>,
    deadline: Arc<Mutex<Option<Instant>>>,
    timed_out: Arc<AtomicBool>,
}

impl JavaScriptRepl {
    fn new() -> anyhow::Result<Self> {
        let bridge = Arc::new(Mutex::new(NativeComputerUseClient::new()));
        let call_output = Arc::new(Mutex::new(None));
        let deadline = Arc::new(Mutex::new(None));
        let timed_out = Arc::new(AtomicBool::new(false));
        let kernel = create_kernel(
            bridge.clone(),
            call_output.clone(),
            deadline.clone(),
            timed_out.clone(),
        )?;
        Ok(Self {
            kernel,
            bridge,
            call_output,
            deadline,
            timed_out,
        })
    }

    fn reset(&mut self) -> anyhow::Result<()> {
        self.kernel = create_kernel(
            self.bridge.clone(),
            self.call_output.clone(),
            self.deadline.clone(),
            self.timed_out.clone(),
        )?;
        Ok(())
    }

    fn execute(&mut self, code: &str, timeout: Duration, request_meta: JsonValue) -> JsonValue {
        self.timed_out.store(false, Ordering::Release);
        *self.deadline.lock() = Some(Instant::now() + timeout);
        *self.call_output.lock() = Some(CallOutput {
            request_meta,
            ..Default::default()
        });

        let execution = self
            .kernel
            .context
            .with(|ctx| -> Result<(), (rquickjs::Error, String)> {
                install_request_meta(&ctx, &self.call_output).map_err(|error| {
                    let message = javascript_error(&ctx, &error);
                    (error, message)
                })?;
                let mut options = EvalOptions::default();
                options.global = true;
                options.promise = true;
                let promise: Promise<'_> =
                    ctx.eval_with_options(code, options).map_err(|error| {
                        let message = javascript_error(&ctx, &error);
                        (error, message)
                    })?;
                promise.finish::<Value<'_>>().map(|_| ()).map_err(|error| {
                    let message = javascript_error(&ctx, &error);
                    (error, message)
                })
            });

        *self.deadline.lock() = None;
        let mut output = self.call_output.lock().take().unwrap_or_default();
        while output.text.ends_with('\n') {
            output.text.pop();
        }
        match execution {
            Ok(()) => tool_result(&output.text, false, output.images, output.response_meta),
            Err((_error, mut message)) => {
                if self.timed_out.load(Ordering::Acquire) {
                    message = format!(
                        "JavaScript execution timed out after {} ms",
                        timeout.as_millis()
                    );
                }
                if !output.text.is_empty() {
                    message = format!("{}\n{}", output.text, message);
                }
                tool_result(&message, true, output.images, output.response_meta)
            }
        }
    }
}

fn create_kernel(
    bridge: Arc<Mutex<NativeComputerUseClient>>,
    call_output: Arc<Mutex<Option<CallOutput>>>,
    deadline: Arc<Mutex<Option<Instant>>>,
    timed_out: Arc<AtomicBool>,
) -> anyhow::Result<Kernel> {
    let runtime = Runtime::new().context("could not create the QuickJS runtime")?;
    let interrupt_deadline = deadline.clone();
    let interrupt_timed_out = timed_out.clone();
    runtime.set_interrupt_handler(Some(Box::new(move || {
        let expired = interrupt_deadline
            .lock()
            .is_some_and(|deadline| Instant::now() >= deadline);
        if expired {
            interrupt_timed_out.store(true, Ordering::Release);
        }
        expired
    })));
    runtime.set_memory_limit(256 * 1024 * 1024);
    runtime.set_max_stack_size(2 * 1024 * 1024);
    let context = Context::full(&runtime).context("could not create the QuickJS context")?;
    context.with(|ctx| -> anyhow::Result<()> {
        let globals = ctx.globals();

        let sky_bridge = bridge.clone();
        let sky_deadline = deadline.clone();
        globals.set(
            "__wakuSkyCall",
            Function::new(ctx.clone(), move |name: String, arguments: String| {
                let deadline = *sky_deadline.lock();
                let result = serde_json::from_str::<JsonValue>(&arguments)
                    .context("Computer Use arguments are invalid JSON")
                    .and_then(|arguments| sky_bridge.lock().call_sky(&name, arguments, deadline));
                match result {
                    Ok(value) => json!({"ok": true, "value": value}).to_string(),
                    Err(error) => json!({"ok": false, "error": error.to_string()}).to_string(),
                }
            })?,
        )?;

        let write_output = call_output.clone();
        globals.set(
            "__wakuWrite",
            Function::new(ctx.clone(), move |text: String, newline: bool| {
                let mut active = write_output.lock();
                let Some(output) = active.as_mut() else {
                    return false;
                };
                output.text.push_str(&text);
                if newline {
                    output.text.push('\n');
                }
                true
            })?,
        )?;

        let image_output = call_output.clone();
        globals.set(
            "__wakuEmitImage",
            Function::new(ctx.clone(), move |image: String| {
                let result = decode_image_reference(&image).and_then(|image| {
                    let mut active = image_output.lock();
                    let output = active
                        .as_mut()
                        .ok_or_else(|| anyhow!("no JavaScript execution is active"))?;
                    output.images.push(image);
                    Ok(())
                });
                match result {
                    Ok(()) => json!({"ok": true}).to_string(),
                    Err(error) => json!({"ok": false, "error": error.to_string()}).to_string(),
                }
            })?,
        )?;

        let metadata_output = call_output.clone();
        globals.set(
            "__wakuSetResponseMeta",
            Function::new(ctx.clone(), move |metadata: String| {
                let result = serde_json::from_str::<JsonValue>(&metadata)
                    .context("response metadata is invalid JSON")
                    .and_then(|metadata| {
                        let metadata = metadata
                            .as_object()
                            .ok_or_else(|| anyhow!("response metadata must be an object"))?;
                        let mut active = metadata_output.lock();
                        let output = active
                            .as_mut()
                            .ok_or_else(|| anyhow!("no JavaScript execution is active"))?;
                        output.response_meta.extend(metadata.clone());
                        Ok(())
                    });
                match result {
                    Ok(()) => json!({"ok": true}).to_string(),
                    Err(error) => json!({"ok": false, "error": error.to_string()}).to_string(),
                }
            })?,
        )?;

        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let home = dirs::home_dir().unwrap_or_default();
        let temp = std::env::temp_dir();
        globals.set("__wakuCwd", cwd.display().to_string())?;
        globals.set("__wakuHomeDir", home.display().to_string())?;
        globals.set("__wakuTmpDir", temp.display().to_string())?;
        ctx.eval::<(), _>(KERNEL_BOOTSTRAP)?;
        Ok(())
    })?;
    Ok(Kernel {
        _runtime: runtime,
        context,
    })
}

fn install_request_meta(
    ctx: &Ctx<'_>,
    call_output: &Arc<Mutex<Option<CallOutput>>>,
) -> rquickjs::Result<()> {
    let request_meta = call_output
        .lock()
        .as_ref()
        .map(|output| output.request_meta.clone())
        .unwrap_or_else(|| json!({}));
    let json = serde_json::to_string(&request_meta).unwrap_or_else(|_| "{}".into());
    let json_literal = serde_json::to_string(&json).unwrap_or_else(|_| "\"{}\"".into());
    ctx.eval::<(), _>(format!(
        "globalThis.nodeRepl.requestMeta = JSON.parse({json_literal});"
    ))
}

fn javascript_error(ctx: &Ctx<'_>, error: &rquickjs::Error) -> String {
    if matches!(error, rquickjs::Error::Exception) {
        let caught = ctx.catch();
        if let Ok(exception) = Exception::from_value(caught.clone()) {
            return exception
                .message()
                .or_else(|| exception.stack())
                .unwrap_or_else(|| "JavaScript execution failed".into());
        }
        if let Some(string) = caught.as_string()
            && let Ok(string) = string.to_string()
        {
            return string;
        }
    }
    error.to_string()
}

fn tool_result(
    text: &str,
    is_error: bool,
    images: Vec<EmittedImage>,
    response_meta: Map<String, JsonValue>,
) -> JsonValue {
    let mut content = vec![json!({"type": "text", "text": text})];
    content.extend(images.into_iter().map(|image| {
        json!({
            "type": "image",
            "data": image.data,
            "mimeType": image.mime_type
        })
    }));
    let mut result = json!({"content": content, "isError": is_error});
    if !response_meta.is_empty() {
        result["_meta"] = JsonValue::Object(response_meta);
    }
    result
}

fn decode_image_reference(reference: &str) -> anyhow::Result<EmittedImage> {
    if let Some(data) = reference.strip_prefix("data:") {
        let (header, payload) = data
            .split_once(',')
            .ok_or_else(|| anyhow!("image data URL has no payload"))?;
        let mime_type = header
            .strip_suffix(";base64")
            .ok_or_else(|| anyhow!("image data URL must use base64"))?;
        if !mime_type.starts_with("image/") {
            bail!("emitImage only accepts image data");
        }
        base64::engine::general_purpose::STANDARD
            .decode(payload)
            .context("image data URL contains invalid base64")?;
        return Ok(EmittedImage {
            data: payload.into(),
            mime_type: mime_type.into(),
        });
    }
    if let Some(path) = reference.strip_prefix("file://") {
        let path = percent_decode_path(path)?;
        let bytes =
            fs::read(&path).with_context(|| format!("could not read image {}", path.display()))?;
        let mime_type = image_mime_type(&path, &bytes)?;
        return Ok(EmittedImage {
            data: base64::engine::general_purpose::STANDARD.encode(bytes),
            mime_type: mime_type.into(),
        });
    }
    bail!("emitImage expects a base64 data URL or file URL")
}

fn percent_decode_path(path: &str) -> anyhow::Result<PathBuf> {
    let mut bytes = Vec::with_capacity(path.len());
    let bytes_in = path.as_bytes();
    let mut index = 0;
    while index < bytes_in.len() {
        if bytes_in[index] == b'%' {
            if index + 2 >= bytes_in.len() {
                bail!("file URL contains invalid percent encoding");
            }
            let hex = std::str::from_utf8(&bytes_in[index + 1..index + 3])?;
            bytes.push(
                u8::from_str_radix(hex, 16)
                    .context("file URL contains invalid percent encoding")?,
            );
            index += 3;
        } else {
            bytes.push(bytes_in[index]);
            index += 1;
        }
    }
    Ok(PathBuf::from(
        String::from_utf8(bytes).context("file URL is not UTF-8")?,
    ))
}

fn image_mime_type(path: &Path, bytes: &[u8]) -> anyhow::Result<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Ok("image/png");
    }
    if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        return Ok("image/jpeg");
    }
    if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Ok("image/webp");
    }
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("gif") => Ok("image/gif"),
        Some("svg") => Ok("image/svg+xml"),
        _ => bail!("could not determine the image MIME type"),
    }
}

struct NativeComputerUseClient {
    connection: Option<HelperConnection>,
}

impl NativeComputerUseClient {
    fn new() -> Self {
        Self { connection: None }
    }

    fn call_sky(
        &mut self,
        name: &str,
        arguments: JsonValue,
        deadline: Option<Instant>,
    ) -> anyhow::Result<JsonValue> {
        if !matches!(
            name,
            "list_apps"
                | "get_app_state"
                | "click"
                | "drag"
                | "perform_secondary_action"
                | "set_value"
                | "select_text"
                | "scroll"
                | "press_key"
                | "type_text"
        ) {
            bail!("unknown sky operation: {name}");
        }
        let requested_app = arguments.get("app").cloned().unwrap_or(JsonValue::Null);
        let result = self.call_helper(name, arguments, deadline)?;
        match name {
            "list_apps" => {
                if let Some(apps) = result.pointer("/structuredContent/apps") {
                    return Ok(apps.clone());
                }
                let text = text_content(&result);
                serde_json::from_str(&text).context("Computer Use returned an invalid app list")
            }
            "get_app_state" => {
                let structured = result
                    .get("structuredContent")
                    .and_then(JsonValue::as_object);
                let app = structured
                    .and_then(|value| value.get("app"))
                    .cloned()
                    .unwrap_or(requested_app);
                let text = structured
                    .and_then(|value| value.get("text"))
                    .and_then(JsonValue::as_str)
                    .map(str::to_owned)
                    .unwrap_or_else(|| text_content(&result));
                let screenshot = structured
                    .and_then(|value| value.get("screenshot"))
                    .and_then(JsonValue::as_str)
                    .map(|url| json!({"url": url}))
                    .unwrap_or(JsonValue::Null);
                Ok(json!({"app": app, "text": text, "screenshot": screenshot}))
            }
            _ => Ok(JsonValue::Null),
        }
    }

    fn call_helper(
        &mut self,
        name: &str,
        arguments: JsonValue,
        deadline: Option<Instant>,
    ) -> anyhow::Result<JsonValue> {
        if self.connection.is_none() {
            self.connection = Some(HelperConnection::start(deadline)?);
        }
        let result = self
            .connection
            .as_mut()
            .expect("initialized above")
            .call(name, arguments, deadline);
        if result.is_err() {
            self.connection = None;
        }
        result
    }
}

struct HelperConnection {
    child: Child,
    input: BufWriter<ChildStdin>,
    output: BufReader<ChildStdout>,
    next_id: u64,
}

struct RequestWatchdog {
    completed: Option<std::sync::mpsc::Sender<()>>,
    thread: Option<std::thread::JoinHandle<bool>>,
}

impl RequestWatchdog {
    fn start(pid: u32, deadline: Option<Instant>) -> anyhow::Result<Self> {
        let Some(deadline) = deadline else {
            return Ok(Self {
                completed: None,
                thread: None,
            });
        };
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(|| anyhow!("Computer Use request timed out"))?;
        let (completed, completion) = std::sync::mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("waku-js-repl-computer-use-timeout".into())
            .spawn(move || {
                if completion.recv_timeout(remaining).is_ok() {
                    return false;
                }
                let _ = Command::new("/bin/kill")
                    .args(["-TERM", &pid.to_string()])
                    .status();
                true
            })?;
        Ok(Self {
            completed: Some(completed),
            thread: Some(thread),
        })
    }

    fn finish(mut self) -> bool {
        self.complete()
    }

    fn complete(&mut self) -> bool {
        if let Some(completed) = self.completed.take() {
            let _ = completed.send(());
        }
        self.thread
            .take()
            .and_then(|thread| thread.join().ok())
            .unwrap_or(false)
    }
}

impl Drop for RequestWatchdog {
    fn drop(&mut self) {
        let _ = self.complete();
    }
}

impl HelperConnection {
    fn start(deadline: Option<Instant>) -> anyhow::Result<Self> {
        let command = std::env::var_os("WAKU_COMPUTER_USE_SERVER")
            .map(PathBuf::from)
            .ok_or_else(|| {
                anyhow!("WAKU_COMPUTER_USE_SERVER is required before the first sky operation")
            })?;
        let mut child = Command::new(&command)
            .arg("mcp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("failed to start {}", command.display()))?;
        let input = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("Computer Use helper stdin is unavailable"))?;
        let output = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("Computer Use helper stdout is unavailable"))?;
        if let Some(stderr) = child.stderr.take() {
            std::thread::Builder::new()
                .name("waku-js-repl-computer-use-stderr".into())
                .spawn(move || {
                    for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                        eprintln!("Waku Computer Use: {line}");
                    }
                })?;
        }
        let mut connection = Self {
            child,
            input: BufWriter::new(input),
            output: BufReader::new(output),
            next_id: 1,
        };
        connection.request(
            "initialize",
            json!({
                "protocolVersion": MCP_PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": "waku_js_repl", "version": env!("CARGO_PKG_VERSION")}
            }),
            deadline,
        )?;
        connection.notify("notifications/initialized", json!({}))?;
        Ok(connection)
    }

    fn call(
        &mut self,
        name: &str,
        arguments: JsonValue,
        deadline: Option<Instant>,
    ) -> anyhow::Result<JsonValue> {
        let result = self.request(
            "tools/call",
            json!({"name": name, "arguments": arguments}),
            deadline,
        )?;
        if result.get("isError").and_then(JsonValue::as_bool) == Some(true) {
            bail!("{}", text_content(&result));
        }
        Ok(result)
    }

    fn request(
        &mut self,
        method: &str,
        params: JsonValue,
        deadline: Option<Instant>,
    ) -> anyhow::Result<JsonValue> {
        let id = self.next_id;
        self.next_id += 1;
        let watchdog = RequestWatchdog::start(self.child.id(), deadline)?;
        let result = (|| -> anyhow::Result<JsonValue> {
            write_message(
                &mut self.input,
                &json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}),
            )?;
            loop {
                let mut line = String::new();
                let bytes = self.output.read_line(&mut line)?;
                if bytes == 0 {
                    let status = self.child.try_wait()?;
                    bail!(
                        "Computer Use helper closed its session{}",
                        status
                            .map(|status| format!(" ({status})"))
                            .unwrap_or_default()
                    );
                }
                let message: JsonValue = serde_json::from_str(line.trim())
                    .context("Computer Use helper returned invalid JSON")?;
                if message.get("id").and_then(JsonValue::as_u64) != Some(id) {
                    continue;
                }
                if let Some(error) = message.get("error") {
                    let detail = error
                        .get("message")
                        .and_then(JsonValue::as_str)
                        .unwrap_or("Computer Use request failed");
                    bail!("{detail}");
                }
                return message
                    .get("result")
                    .cloned()
                    .ok_or_else(|| anyhow!("Computer Use response has no result"));
            }
        })();
        if watchdog.finish() {
            bail!("Computer Use request timed out");
        }
        result
    }

    fn notify(&mut self, method: &str, params: JsonValue) -> anyhow::Result<()> {
        write_message(
            &mut self.input,
            &json!({"jsonrpc": "2.0", "method": method, "params": params}),
        )
    }
}

impl Drop for HelperConnection {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn text_content(result: &JsonValue) -> String {
    result
        .get("content")
        .and_then(JsonValue::as_array)
        .into_iter()
        .flatten()
        .filter(|item| item.get("type").and_then(JsonValue::as_str) == Some("text"))
        .filter_map(|item| item.get("text").and_then(JsonValue::as_str))
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn write_message(output: &mut impl Write, message: &JsonValue) -> anyhow::Result<()> {
    serde_json::to_writer(&mut *output, message)?;
    output.write_all(b"\n")?;
    output.flush()?;
    Ok(())
}

fn json_rpc_result(id: JsonValue, result: JsonValue) -> JsonValue {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn json_rpc_error(id: JsonValue, code: i64, message: &str) -> JsonValue {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {"code": code, "message": message}
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(repl: &mut JavaScriptRepl, code: &str) -> JsonValue {
        repl.execute(code, Duration::from_secs(1), json!({}))
    }

    #[test]
    fn tool_list_matches_the_supported_repl_surface() {
        let tools = tool_definitions();
        assert_eq!(
            tools
                .iter()
                .filter_map(|tool| tool.get("name").and_then(JsonValue::as_str))
                .collect::<Vec<_>>(),
            ["js", "js_reset"]
        );
    }

    #[test]
    fn repl_persists_global_bindings_and_reset_clears_them() {
        let mut repl = JavaScriptRepl::new().unwrap();
        assert_eq!(
            call(&mut repl, "var persistentValue = 41;")["isError"],
            false
        );
        assert_eq!(
            call(&mut repl, "nodeRepl.write(String(persistentValue + 1));")["content"][0]["text"],
            "42"
        );
        repl.reset().unwrap();
        assert_eq!(call(&mut repl, "persistentValue")["isError"], true);
    }

    #[test]
    fn repl_supports_top_level_await_and_lazy_native_sky() {
        let mut repl = JavaScriptRepl::new().unwrap();
        let initial = call(&mut repl, "nodeRepl.write(typeof sky);");
        assert_eq!(initial["content"][0]["text"], "undefined");
        let result = call(
            &mut repl,
            "await setupComputerUseRuntime({ globals: globalThis }); var promisedValue = await Promise.resolve(7); nodeRepl.write(`${sky.target}:${promisedValue}`);",
        );
        assert_eq!(result["isError"], false);
        assert_eq!(result["content"][0]["text"], "mac:7");
    }

    #[test]
    fn repl_interrupts_runaway_javascript() {
        let mut repl = JavaScriptRepl::new().unwrap();
        let result = repl.execute("while (true) {}", Duration::from_millis(10), json!({}));
        assert_eq!(result["isError"], true);
        assert!(
            result["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("timed out")
        );
    }

    #[test]
    fn repl_forwards_request_metadata_and_emitted_images() {
        let mut repl = JavaScriptRepl::new().unwrap();
        let result = repl.execute(
            r#"
                nodeRepl.write(nodeRepl.requestMeta.label);
                nodeRepl.setResponseMeta({ source: "quickjs" });
                await nodeRepl.emitImage("data:image/png;base64,aGVsbG8=");
            "#,
            Duration::from_secs(1),
            json!({"label": "metadata"}),
        );
        assert_eq!(result["isError"], false);
        assert_eq!(result["content"][0]["text"], "metadata");
        assert_eq!(result["content"][1]["type"], "image");
        assert_eq!(result["content"][1]["data"], "aGVsbG8=");
        assert_eq!(result["_meta"]["source"], "quickjs");
    }

    #[test]
    fn data_url_images_become_mcp_image_content() {
        let image = decode_image_reference("data:image/png;base64,aGVsbG8=").unwrap();
        assert_eq!(image.mime_type, "image/png");
        assert_eq!(image.data, "aGVsbG8=");
    }
}

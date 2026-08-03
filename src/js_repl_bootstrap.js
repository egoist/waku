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
  const nativeScheduleTimer = globalThis.__wakuScheduleTimer;
  const nativeClearTimer = globalThis.__wakuClearTimer;
  const nativeRefreshTimer = globalThis.__wakuRefreshTimer;
  const cwd = globalThis.__wakuCwd;
  const homeDir = globalThis.__wakuHomeDir;
  const tmpDir = globalThis.__wakuTmpDir;
  delete globalThis.__wakuSkyCall;
  delete globalThis.__wakuWrite;
  delete globalThis.__wakuEmitImage;
  delete globalThis.__wakuSetResponseMeta;
  delete globalThis.__wakuScheduleTimer;
  delete globalThis.__wakuClearTimer;
  delete globalThis.__wakuRefreshTimer;
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

  const timerId = Symbol("waku.timerId");
  const timerRefed = Symbol("waku.timerRefed");
  const timers = new Map();
  class Timeout {
    constructor(id) {
      Object.defineProperty(this, timerId, { value: id });
      this[timerRefed] = true;
    }
    ref() { this[timerRefed] = true; return this; }
    unref() { this[timerRefed] = false; return this; }
    hasRef() { return this[timerRefed]; }
    refresh() { nativeRefreshTimer(this[timerId]); return this; }
    close() { clearTimer(this); return this; }
    [Symbol.toPrimitive]() { return this[timerId]; }
  }
  const normalizeDelay = (delay) => {
    const milliseconds = Number(delay);
    if (!Number.isFinite(milliseconds) || milliseconds < 1 || milliseconds > 2147483647) {
      return 1;
    }
    return Math.trunc(milliseconds);
  };
  const scheduleTimer = (callback, delay, repeat, args) => {
    if (typeof callback !== "function") {
      throw new TypeError('The "callback" argument must be of type function');
    }
    const id = nativeScheduleTimer(normalizeDelay(delay), repeat);
    const handle = new Timeout(id);
    timers.set(id, { callback, args, repeat, handle });
    return handle;
  };
  const timerIdFrom = (handle) => {
    if (handle instanceof Timeout) return handle[timerId];
    const id = Number(handle);
    return Number.isSafeInteger(id) && id > 0 ? id : undefined;
  };
  const clearTimer = (handle) => {
    const id = timerIdFrom(handle);
    if (id === undefined) return;
    timers.delete(id);
    nativeClearTimer(id);
  };
  globalThis.__wakuRunTimer = (id) => {
    const timer = timers.get(id);
    if (!timer) return;
    if (!timer.repeat) timers.delete(id);
    timer.callback(...timer.args);
  };
  globalThis.setTimeout = (callback, delay = 1, ...args) =>
    scheduleTimer(callback, delay, false, args);
  globalThis.clearTimeout = clearTimer;
  globalThis.setInterval = (callback, delay = 1, ...args) =>
    scheduleTimer(callback, delay, true, args);
  globalThis.clearInterval = clearTimer;
  globalThis.setImmediate = (callback, ...args) => scheduleTimer(callback, 1, false, args);
  globalThis.clearImmediate = clearTimer;
  if (Symbol.dispose) {
    Object.defineProperty(Timeout.prototype, Symbol.dispose, {
      value() { clearTimer(this); },
    });
  }
  if (typeof globalThis.queueMicrotask !== "function") {
    globalThis.queueMicrotask = (callback) => {
      if (typeof callback !== "function") {
        throw new TypeError('The "callback" argument must be of type function');
      }
      Promise.resolve().then(callback);
    };
  }

  const utf8Encode = (input) => {
    const output = [];
    const string = String(input);
    for (let index = 0; index < string.length; index += 1) {
      let codePoint = string.charCodeAt(index);
      if (codePoint >= 0xd800 && codePoint <= 0xdbff) {
        const low = string.charCodeAt(index + 1);
        if (low >= 0xdc00 && low <= 0xdfff) {
          codePoint = 0x10000 + ((codePoint - 0xd800) << 10) + (low - 0xdc00);
          index += 1;
        } else {
          codePoint = 0xfffd;
        }
      } else if (codePoint >= 0xdc00 && codePoint <= 0xdfff) {
        codePoint = 0xfffd;
      }
      if (codePoint <= 0x7f) output.push(codePoint);
      else if (codePoint <= 0x7ff) {
        output.push(0xc0 | (codePoint >> 6), 0x80 | (codePoint & 0x3f));
      } else if (codePoint <= 0xffff) {
        output.push(0xe0 | (codePoint >> 12), 0x80 | ((codePoint >> 6) & 0x3f), 0x80 | (codePoint & 0x3f));
      } else {
        output.push(0xf0 | (codePoint >> 18), 0x80 | ((codePoint >> 12) & 0x3f), 0x80 | ((codePoint >> 6) & 0x3f), 0x80 | (codePoint & 0x3f));
      }
    }
    return output;
  };
  const utf8Decode = (bytes) => {
    let output = "";
    for (let index = 0; index < bytes.length;) {
      const first = bytes[index++];
      let codePoint;
      let remaining;
      if (first <= 0x7f) { codePoint = first; remaining = 0; }
      else if (first >= 0xc2 && first <= 0xdf) { codePoint = first & 0x1f; remaining = 1; }
      else if (first >= 0xe0 && first <= 0xef) { codePoint = first & 0x0f; remaining = 2; }
      else if (first >= 0xf0 && first <= 0xf4) { codePoint = first & 0x07; remaining = 3; }
      else { output += "\ufffd"; continue; }
      if (index + remaining > bytes.length) { output += "\ufffd"; break; }
      let valid = true;
      for (let offset = 0; offset < remaining; offset += 1) {
        const byte = bytes[index + offset];
        if ((byte & 0xc0) !== 0x80) { valid = false; break; }
        codePoint = (codePoint << 6) | (byte & 0x3f);
      }
      if (!valid) { output += "\ufffd"; continue; }
      index += remaining;
      if (
        (remaining === 1 && codePoint < 0x80) ||
        (remaining === 2 && codePoint < 0x800) ||
        (remaining === 3 && codePoint < 0x10000) ||
        codePoint > 0x10ffff ||
        (codePoint >= 0xd800 && codePoint <= 0xdfff)
      ) {
        output += "\ufffd";
      } else {
        output += String.fromCodePoint(codePoint);
      }
    }
    return output;
  };
  const base64Alphabet = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
  const base64Encode = (bytes, urlSafe = false) => {
    let output = "";
    for (let index = 0; index < bytes.length; index += 3) {
      const remaining = bytes.length - index;
      const value = (bytes[index] << 16) | ((bytes[index + 1] ?? 0) << 8) | (bytes[index + 2] ?? 0);
      output += base64Alphabet[(value >> 18) & 63];
      output += base64Alphabet[(value >> 12) & 63];
      output += remaining > 1 ? base64Alphabet[(value >> 6) & 63] : "=";
      output += remaining > 2 ? base64Alphabet[value & 63] : "=";
    }
    if (urlSafe) return output.replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
    return output;
  };
  const base64Decode = (input) => {
    const string = String(input).replace(/[\t\n\f\r ]/g, "").replace(/-/g, "+").replace(/_/g, "/");
    const output = [];
    let value = 0;
    let bits = 0;
    for (const character of string) {
      if (character === "=") break;
      const digit = base64Alphabet.indexOf(character);
      if (digit < 0) continue;
      value = (value << 6) | digit;
      bits += 6;
      if (bits >= 8) {
        bits -= 8;
        output.push((value >> bits) & 0xff);
      }
    }
    return output;
  };
  const normalizeEncoding = (encoding = "utf8") => String(encoding).toLowerCase().replace(/[-_]/g, "");
  const encodeString = (value, encoding) => {
    switch (normalizeEncoding(encoding)) {
      case "utf8": return utf8Encode(value);
      case "base64":
      case "base64url": return base64Decode(value);
      case "hex": {
        const string = String(value);
        const output = [];
        for (let index = 0; index + 1 < string.length; index += 2) {
          const byte = Number.parseInt(string.slice(index, index + 2), 16);
          if (!Number.isFinite(byte)) break;
          output.push(byte);
        }
        return output;
      }
      case "latin1":
      case "binary": return Array.from(String(value), (character) => character.charCodeAt(0) & 0xff);
      case "ascii": return Array.from(String(value), (character) => character.charCodeAt(0) & 0x7f);
      default: throw new TypeError(`Unknown encoding: ${encoding}`);
    }
  };
  const decodeBytes = (bytes, encoding) => {
    switch (normalizeEncoding(encoding)) {
      case "utf8": return utf8Decode(bytes);
      case "base64": return base64Encode(bytes);
      case "base64url": return base64Encode(bytes, true);
      case "hex": return Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
      case "latin1":
      case "binary": return Array.from(bytes, (byte) => String.fromCharCode(byte)).join("");
      case "ascii": return Array.from(bytes, (byte) => String.fromCharCode(byte & 0x7f)).join("");
      default: throw new TypeError(`Unknown encoding: ${encoding}`);
    }
  };
  class Buffer extends Uint8Array {
    static from(value, encodingOrOffset, length) {
      if (typeof value === "string") return new Buffer(encodeString(value, encodingOrOffset));
      if (value instanceof ArrayBuffer) {
        const offset = Number(encodingOrOffset ?? 0);
        const size = length === undefined ? value.byteLength - offset : Number(length);
        return new Buffer(value, offset, size);
      }
      if (ArrayBuffer.isView(value)) {
        return new Buffer(new Uint8Array(value.buffer, value.byteOffset, value.byteLength));
      }
      if (value?.type === "Buffer" && Array.isArray(value.data)) return new Buffer(value.data);
      if (value != null && (Array.isArray(value) || typeof value[Symbol.iterator] === "function")) {
        return new Buffer(Array.from(value));
      }
      throw new TypeError("Buffer.from expects a string, ArrayBuffer, view, or iterable");
    }
    static alloc(size, fill = 0, encoding) {
      const buffer = new Buffer(Number(size));
      if (typeof fill === "string") {
        const pattern = Buffer.from(fill, encoding);
        if (pattern.length > 0) {
          for (let index = 0; index < buffer.length; index += 1) buffer[index] = pattern[index % pattern.length];
        }
      } else {
        buffer.fill(Number(fill) & 0xff);
      }
      return buffer;
    }
    static allocUnsafe(size) { return Buffer.alloc(size); }
    static allocUnsafeSlow(size) { return Buffer.alloc(size); }
    static byteLength(value, encoding) { return Buffer.from(value, encoding).byteLength; }
    static isBuffer(value) { return value instanceof Buffer; }
    static concat(list, totalLength) {
      if (!Array.isArray(list)) throw new TypeError("list must be an Array of Buffers");
      const size = totalLength === undefined
        ? list.reduce((sum, item) => sum + item.byteLength, 0)
        : Number(totalLength);
      const output = Buffer.alloc(size);
      let offset = 0;
      for (const item of list) {
        const bytes = Buffer.from(item);
        output.set(bytes.subarray(0, Math.max(0, output.length - offset)), offset);
        offset += bytes.length;
        if (offset >= output.length) break;
      }
      return output;
    }
    toString(encoding = "utf8", start = 0, end = this.length) {
      return decodeBytes(this.subarray(Math.max(0, start), Math.min(this.length, end)), encoding);
    }
    write(string, offset = 0, length = this.length - offset, encoding = "utf8") {
      const bytes = Buffer.from(string, encoding).subarray(0, length);
      this.set(bytes, offset);
      return bytes.length;
    }
    equals(other) {
      const bytes = Buffer.from(other);
      return this.length === bytes.length && this.every((byte, index) => byte === bytes[index]);
    }
    toJSON() { return { type: "Buffer", data: Array.from(this) }; }
  }
  globalThis.Buffer = Buffer;
  globalThis.atob = (value) => decodeBytes(base64Decode(value), "latin1");
  globalThis.btoa = (value) => {
    const bytes = encodeString(value, "latin1");
    if (Array.from(String(value)).some((character) => character.charCodeAt(0) > 0xff)) {
      throw new TypeError("Invalid character");
    }
    return base64Encode(bytes);
  };
  if (typeof globalThis.TextEncoder !== "function") {
    globalThis.TextEncoder = class TextEncoder {
      get encoding() { return "utf-8"; }
      encode(input = "") { return new Uint8Array(utf8Encode(input)); }
      encodeInto(input, destination) {
        const bytes = utf8Encode(input);
        const written = Math.min(bytes.length, destination.length);
        destination.set(bytes.slice(0, written));
        return { read: String(input).length, written };
      }
    };
  }
  if (typeof globalThis.TextDecoder !== "function") {
    globalThis.TextDecoder = class TextDecoder {
      constructor(label = "utf-8") {
        if (normalizeEncoding(label) !== "utf8") throw new TypeError("Only UTF-8 is supported");
      }
      get encoding() { return "utf-8"; }
      decode(input = new Uint8Array()) { return utf8Decode(Buffer.from(input)); }
    };
  }

  globalThis.global = globalThis;
  globalThis.tmpDir = tmpDir;

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

  let requestMeta = Object.freeze({});
  globalThis.__wakuSetRequestMeta = (meta) => {
    requestMeta = Object.freeze(meta ?? {});
  };
  globalThis.nodeRepl = Object.freeze({
    cwd,
    env: Object.freeze({}),
    homeDir,
    tmpDir,
    get requestMeta() { return requestMeta; },
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
  });
})();

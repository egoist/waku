import {
  PROTOCOL_VERSION,
  type AutomationNotification,
  type ClientMessage,
  type Command,
  type ReplayCursor,
  type ResponsePayload,
  type SequencedEvent,
  type ServerMessage,
} from "./generated";

const NIL_UUID = "00000000-0000-0000-0000-000000000000";
const OPEN = 1;
const MAX_BUFFERED_EVENTS_PER_RUNTIME = 4096;
const MAX_BUFFERED_AUTOMATION_NOTIFICATIONS = 256;

export type EventListener = (event: SequencedEvent) => void;

export interface WebSocketLike {
  readonly readyState: number;
  send(data: string): void;
  close(code?: number, reason?: string): void;
  addEventListener(type: "open", listener: () => void): void;
  addEventListener(type: "message", listener: (event: MessageEvent) => void): void;
  addEventListener(type: "error", listener: () => void): void;
  addEventListener(type: "close", listener: (event: CloseEvent) => void): void;
}

export interface WakuClientOptions {
  /** A daemon address (`127.0.0.1:34123`) or complete ws(s) URL. */
  address: string;
  token: string;
  clientId?: string;
  requestTimeoutMs?: number;
  webSocketFactory?: (url: string) => WebSocketLike;
  randomUUID?: () => string;
}

export class WakuRpcError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "WakuRpcError";
  }
}

interface PendingRequest {
  resolve: (payload: ResponsePayload) => void;
  reject: (error: Error) => void;
  timeout: ReturnType<typeof setTimeout>;
}

interface LastSequence {
  epoch: string;
  sequence: number;
}

type ConnectionState = "disconnected" | "connecting" | "connected";

/** Browser-safe client for Waku's versioned JSON-over-WebSocket protocol. */
export class WakuClient {
  readonly clientId: string;

  private readonly address: string;
  private readonly token: string;
  private readonly requestTimeoutMs: number;
  private readonly socketFactory: (url: string) => WebSocketLike;
  private readonly randomUUID: () => string;
  private socket?: WebSocketLike;
  private state: ConnectionState = "disconnected";
  private pending = new Map<string, PendingRequest>();
  private subscriptions = new Map<string, Set<EventListener>>();
  private pendingEvents = new Map<string, SequencedEvent[]>();
  private taskStateListeners = new Set<(revision: number) => void>();
  private automationNotificationListeners = new Set<
    (notification: AutomationNotification) => void
  >();
  private pendingAutomationNotifications: AutomationNotification[] = [];
  private sequences = new Map<string, LastSequence>();
  private connectionGeneration = 0;
  private rejectConnect?: (error: Error) => void;

  constructor(options: WakuClientOptions) {
    this.address = options.address;
    this.token = options.token;
    this.requestTimeoutMs = options.requestTimeoutMs ?? 120_000;
    this.socketFactory =
      options.webSocketFactory ??
      ((url) => {
        if (typeof WebSocket === "undefined") {
          throw new Error("WebSocket is unavailable; provide webSocketFactory");
        }
        return new WebSocket(url);
      });
    this.randomUUID =
      options.randomUUID ??
      (() => {
        if (typeof crypto === "undefined" || !crypto.randomUUID) {
          throw new Error("crypto.randomUUID is unavailable; provide randomUUID");
        }
        return crypto.randomUUID();
      });
    this.clientId = options.clientId ?? this.randomUUID();
  }

  get connected(): boolean {
    return this.state === "connected";
  }

  /** Connects, or reconnects while replaying events after the last seen sequence. */
  connect(): Promise<void> {
    if (this.state === "connected") return Promise.resolve();
    if (this.state === "connecting") {
      return Promise.reject(new Error("Waku client is already connecting"));
    }

    this.state = "connecting";
    const generation = ++this.connectionGeneration;
    let socket: WebSocketLike;
    try {
      socket = this.socketFactory(daemonUrl(this.address));
    } catch (error) {
      this.state = "disconnected";
      return Promise.reject(asError(error));
    }
    this.socket = socket;

    return new Promise((resolve, reject) => {
      let handshakeSettled = false;
      const failHandshake = (error: Error) => {
        if (handshakeSettled) return;
        handshakeSettled = true;
        if (this.rejectConnect === failHandshake) this.rejectConnect = undefined;
        this.state = "disconnected";
        reject(error);
      };
      this.rejectConnect = failHandshake;

      socket.addEventListener("open", () => {
        if (generation !== this.connectionGeneration) return;
        const hello: ClientMessage = {
          type: "hello",
          protocolVersion: PROTOCOL_VERSION,
          token: this.token,
          clientId: this.clientId,
          resumeFrom: this.replayCursors(),
        };
        socket.send(JSON.stringify(hello));
      });
      socket.addEventListener("message", (event) => {
        if (generation !== this.connectionGeneration) return;
        let message: ServerMessage;
        try {
          message = JSON.parse(String(event.data)) as ServerMessage;
        } catch {
          failHandshake(new Error("Waku daemon sent invalid JSON"));
          return;
        }

        if (!handshakeSettled) {
          if (message.type === "hello") {
            if (message.protocolVersion !== PROTOCOL_VERSION) {
              failHandshake(
                new Error(
                  `daemon protocol ${message.protocolVersion} does not match client protocol ${PROTOCOL_VERSION}`,
                ),
              );
              socket.close(1002, "protocol version mismatch");
              return;
            }
            handshakeSettled = true;
            if (this.rejectConnect === failHandshake) this.rejectConnect = undefined;
            this.state = "connected";
            resolve();
            return;
          }
          if (message.type === "rejected") {
            failHandshake(new Error(`daemon rejected connection: ${message.message}`));
            socket.close(1008, "authentication rejected");
            return;
          }
          failHandshake(new Error("Waku daemon sent an invalid handshake response"));
          socket.close(1002, "invalid handshake");
          return;
        }
        this.handleMessage(message);
      });
      socket.addEventListener("error", () => {
        failHandshake(new Error("Waku daemon connection failed"));
      });
      socket.addEventListener("close", () => {
        if (generation !== this.connectionGeneration) return;
        failHandshake(new Error("Waku daemon disconnected during handshake"));
        this.markDisconnected(new Error("Waku daemon disconnected"));
      });
    });
  }

  request(
    command: Command,
    sessionId = NIL_UUID,
    runtimeId = NIL_UUID,
  ): Promise<ResponsePayload> {
    let socket: WebSocketLike;
    try {
      socket = this.requireSocket();
    } catch (error) {
      return Promise.reject(asError(error));
    }
    const requestId = this.randomUUID();
    const message: ClientMessage = {
      type: "request",
      requestId,
      sessionId,
      runtimeId,
      command,
    };

    return new Promise((resolve, reject) => {
      const timeout = setTimeout(() => {
        this.pending.delete(requestId);
        reject(new Error("timed out waiting for Waku daemon"));
      }, this.requestTimeoutMs);
      this.pending.set(requestId, { resolve, reject, timeout });
      try {
        socket.send(JSON.stringify(message));
      } catch (error) {
        clearTimeout(timeout);
        this.pending.delete(requestId);
        reject(asError(error));
      }
    });
  }

  async notify(
    command: Command,
    sessionId = NIL_UUID,
    runtimeId = NIL_UUID,
  ): Promise<void> {
    const message: ClientMessage = {
      type: "request",
      // The nil request id is the protocol's fire-and-forget marker. Runtime
      // ordering is preserved, but high-frequency controls such as terminal
      // input do not create response traffic or response-cache entries.
      requestId: NIL_UUID,
      sessionId,
      runtimeId,
      command,
    };
    this.requireSocket().send(JSON.stringify(message));
  }

  subscribe(sessionId: string, runtimeId: string, listener: EventListener): () => void {
    const key = subscriptionKey(sessionId, runtimeId);
    let listeners = this.subscriptions.get(key);
    if (!listeners) {
      listeners = new Set();
      this.subscriptions.set(key, listeners);
    }
    listeners.add(listener);
    const buffered = this.pendingEvents.get(key);
    if (buffered) {
      this.pendingEvents.delete(key);
      for (const event of buffered) listener(event);
    }
    return () => {
      listeners?.delete(listener);
      if (listeners?.size === 0) this.subscriptions.delete(key);
    };
  }

  subscribeTaskState(listener: (revision: number) => void): () => void {
    this.taskStateListeners.add(listener);
    return () => this.taskStateListeners.delete(listener);
  }

  subscribeAutomationNotifications(
    listener: (notification: AutomationNotification) => void,
  ): () => void {
    this.automationNotificationListeners.add(listener);
    const pending = this.pendingAutomationNotifications;
    this.pendingAutomationNotifications = [];
    for (const notification of pending) listener(notification);
    return () => this.automationNotificationListeners.delete(listener);
  }

  replayCursors(): ReplayCursor[] {
    return [...this.sequences].map(([key, cursor]) => {
      const [sessionId, runtimeId] = key.split(":", 2) as [string, string];
      return { sessionId, runtimeId, ...cursor };
    });
  }

  /** Closes only this client connection; it never stops a remotely managed daemon. */
  disconnect(): void {
    this.rejectConnect?.(new Error("Waku client disconnected"));
    ++this.connectionGeneration;
    const socket = this.socket;
    this.socket = undefined;
    this.markDisconnected(new Error("Waku client disconnected"));
    socket?.close(1000, "client disconnected");
  }

  /** Explicitly requests daemon shutdown, then closes this connection. */
  shutdownDaemon(): void {
    const socket = this.requireSocket();
    socket.send(JSON.stringify({ type: "shutdown" } satisfies ClientMessage));
    this.disconnect();
  }

  private requireSocket(): WebSocketLike {
    if (this.state !== "connected" || !this.socket || this.socket.readyState !== OPEN) {
      throw new Error("Waku daemon is disconnected");
    }
    return this.socket;
  }

  private handleMessage(message: ServerMessage): void {
    if (message.type === "response") {
      const pending = this.pending.get(message.requestId);
      if (!pending) return;
      this.pending.delete(message.requestId);
      clearTimeout(pending.timeout);
      if (message.outcome.status === "ok") pending.resolve(message.outcome.payload);
      else pending.reject(new WakuRpcError(message.outcome.error.message));
      return;
    }
    if (message.type === "event") {
      const key = subscriptionKey(message.sessionId, message.runtimeId);
      const previous = this.sequences.get(key);
      if (
        previous?.epoch === message.epoch &&
        message.sequence <= previous.sequence
      ) {
        return;
      }
      this.sequences.set(key, {
        epoch: message.epoch,
        sequence: message.sequence,
      });
      const listeners = this.subscriptions.get(key);
      if (listeners?.size) {
        for (const listener of listeners) listener(message);
      } else {
        const buffered = this.pendingEvents.get(key) ?? [];
        buffered.push(message);
        if (buffered.length > MAX_BUFFERED_EVENTS_PER_RUNTIME) {
          buffered.splice(0, buffered.length - MAX_BUFFERED_EVENTS_PER_RUNTIME);
        }
        this.pendingEvents.set(key, buffered);
      }
      return;
    }
    if (message.type === "taskStateChanged") {
      for (const listener of this.taskStateListeners) listener(message.revision);
      return;
    }
    if (message.type === "automationNotification") {
      if (this.automationNotificationListeners.size === 0) {
        this.pendingAutomationNotifications.push(message.notification);
        if (
          this.pendingAutomationNotifications.length > MAX_BUFFERED_AUTOMATION_NOTIFICATIONS
        ) {
          this.pendingAutomationNotifications.shift();
        }
      } else {
        for (const listener of this.automationNotificationListeners) {
          listener(message.notification);
        }
      }
      return;
    }
    if (message.type === "shuttingDown") {
      this.socket?.close(1000, "daemon shutting down");
    }
  }

  private markDisconnected(error: Error): void {
    this.state = "disconnected";
    this.socket = undefined;
    for (const pending of this.pending.values()) {
      clearTimeout(pending.timeout);
      pending.reject(error);
    }
    this.pending.clear();
    this.pendingAutomationNotifications = [];
  }
}

export function daemonUrl(address: string): string {
  const normalized = /^(?:ws|wss):\/\//.test(address) ? address : `ws://${address}`;
  const url = new URL(normalized);
  url.pathname = "/v1";
  url.search = "";
  url.hash = "";
  return url.toString();
}

function subscriptionKey(sessionId: string, runtimeId: string): string {
  return `${sessionId}:${runtimeId}`;
}

function asError(error: unknown): Error {
  return error instanceof Error ? error : new Error(String(error));
}

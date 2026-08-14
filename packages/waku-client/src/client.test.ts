import { describe, expect, test } from "bun:test";

import { WakuClient, WakuRpcError, daemonUrl, type WebSocketLike } from "./client";

class FakeSocket implements WebSocketLike {
  readyState = 0;
  sent: string[] = [];
  private listeners = new Map<string, Array<(...args: any[]) => void>>();

  addEventListener(type: "open", listener: () => void): void;
  addEventListener(type: "message", listener: (event: MessageEvent) => void): void;
  addEventListener(type: "error", listener: () => void): void;
  addEventListener(type: "close", listener: (event: CloseEvent) => void): void;
  addEventListener(type: string, listener: (...args: any[]) => void): void {
    const listeners = this.listeners.get(type) ?? [];
    listeners.push(listener);
    this.listeners.set(type, listeners);
  }

  send(data: string): void {
    this.sent.push(data);
  }

  close(): void {
    this.readyState = 3;
  }

  open(): void {
    this.readyState = 1;
    this.emit("open");
  }

  receive(message: unknown): void {
    this.emit("message", { data: JSON.stringify(message) });
  }

  private emit(type: string, event?: unknown): void {
    for (const listener of this.listeners.get(type) ?? []) listener(event);
  }
}

function fixture() {
  const sockets: FakeSocket[] = [];
  let nextId = 0;
  const client = new WakuClient({
    address: "127.0.0.1:4312",
    token: "secret",
    randomUUID: () => `00000000-0000-4000-8000-${String(++nextId).padStart(12, "0")}`,
    webSocketFactory: () => {
      const socket = new FakeSocket();
      sockets.push(socket);
      return socket;
    },
  });
  return { client, sockets };
}

async function connect(client: WakuClient, socket: FakeSocket): Promise<void> {
  const connected = client.connect();
  socket.open();
  socket.receive({ type: "hello", protocol_version: 1, daemon_version: "test" });
  await connected;
}

describe("WakuClient", () => {
  test("authenticates and correlates typed responses", async () => {
    const { client, sockets } = fixture();
    const connected = client.connect();
    const socket = sockets[0]!;
    socket.open();
    expect(JSON.parse(socket.sent[0]!)).toEqual({
      type: "hello",
      protocol_version: 1,
      token: "secret",
      client_id: "00000000-0000-4000-8000-000000000001",
      resume_from: [],
    });
    socket.receive({ type: "hello", protocol_version: 1, daemon_version: "test" });
    await connected;

    const response = client.request({ type: "getSettings" });
    const request = JSON.parse(socket.sent[1]!);
    socket.receive({
      type: "response",
      request_id: request.request_id,
      outcome: { status: "ok", payload: { type: "ack" } },
    });
    await expect(response).resolves.toEqual({ type: "ack" });
  });

  test("surfaces daemon errors", async () => {
    const { client, sockets } = fixture();
    const socket = sockets[0] ?? new FakeSocket();
    const connected = client.connect();
    const active = sockets[0] ?? socket;
    active.open();
    active.receive({ type: "hello", protocol_version: 1, daemon_version: "test" });
    await connected;

    const response = client.request({ type: "getSettings" });
    const request = JSON.parse(active.sent[1]!);
    active.receive({
      type: "response",
      request_id: request.request_id,
      outcome: { status: "error", error: { message: "nope" } },
    });
    await expect(response).rejects.toBeInstanceOf(WakuRpcError);
  });

  test("deduplicates events and resumes from the last sequence", async () => {
    const { client, sockets } = fixture();
    const firstConnection = client.connect();
    const first = sockets[0]!;
    first.open();
    first.receive({ type: "hello", protocol_version: 1, daemon_version: "test" });
    await firstConnection;

    const received: number[] = [];
    client.subscribe("session", "runtime", (event) => received.push(event.sequence));
    const event = {
      type: "event",
      sessionId: "session",
      runtimeId: "runtime",
      sequence: 4,
      event: { kind: "textDelta", payload: { text: "hi" } },
    };
    first.receive(event);
    first.receive(event);
    expect(received).toEqual([4]);

    client.disconnect();
    const secondConnection = client.connect();
    const second = sockets[1]!;
    second.open();
    expect(JSON.parse(second.sent[0]!).resume_from).toEqual([
      { sessionId: "session", runtimeId: "runtime", sequence: 4 },
    ]);
    second.receive({ type: "hello", protocol_version: 1, daemon_version: "test" });
    await secondConnection;
  });

  test("disconnect rejects an in-flight handshake and permits reconnecting", async () => {
    const { client, sockets } = fixture();
    const firstConnection = client.connect();
    client.disconnect();
    await expect(firstConnection).rejects.toThrow("Waku client disconnected");

    const secondConnection = client.connect();
    const second = sockets[1]!;
    second.open();
    second.receive({ type: "hello", protocol_version: 1, daemon_version: "test" });
    await expect(secondConnection).resolves.toBeUndefined();
  });
});

test("daemonUrl pins the versioned endpoint", () => {
  expect(daemonUrl("localhost:3030/anything?old=1")).toBe("ws://localhost:3030/v1");
  expect(daemonUrl("wss://waku.example.test")).toBe("wss://waku.example.test/v1");
});

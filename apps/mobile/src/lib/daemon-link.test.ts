import { describe, expect, test } from "bun:test";
import { PROTOCOL_VERSION, WakuClient, type WebSocketLike } from "@waku/client";

import { DaemonLink, type DaemonLinkOptions } from "./daemon-link";

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

  fail(reason: string): void {
    this.readyState = 3;
    this.emit("error", {});
    this.emit("close", { reason });
  }

  open(): void {
    this.readyState = 1;
    this.emit("open");
  }

  receive(message: unknown): void {
    this.emit("message", { data: JSON.stringify(message) });
  }

  /** Answers the most recent request with an ack. */
  answerLast(): void {
    const request = JSON.parse(this.sent.at(-1)!) as { requestId: string };
    this.receive({
      type: "response",
      requestId: request.requestId,
      outcome: { status: "ok", payload: { type: "ack" } },
    });
  }

  private emit(type: string, event?: unknown): void {
    for (const listener of this.listeners.get(type) ?? []) listener(event);
  }
}

/** Deterministic clock and timer queue; `advance` runs due timers in order and
 * lets promise chains settle after each. */
class FakeClock {
  now = 1_000_000;
  private seq = 0;
  private timers: Array<{ id: number; at: number; fn: () => void }> = [];

  set = (fn: () => void, delayMs: number): number => {
    const id = ++this.seq;
    this.timers.push({ id, at: this.now + delayMs, fn });
    return id;
  };

  clear = (handle: unknown): void => {
    this.timers = this.timers.filter((timer) => timer.id !== handle);
  };

  get pending(): number[] {
    return this.timers.map((timer) => timer.at - this.now).sort((a, b) => a - b);
  }

  async advance(ms: number): Promise<void> {
    const target = this.now + ms;
    for (;;) {
      const next = this.timers
        .filter((timer) => timer.at <= target)
        .sort((a, b) => a.at - b.at || a.id - b.id)[0];
      if (!next) break;
      this.timers = this.timers.filter((timer) => timer.id !== next.id);
      this.now = next.at;
      next.fn();
      await settle();
    }
    this.now = target;
  }
}

const settle = () => new Promise<void>((resolve) => setTimeout(resolve, 0));
const realWait = (ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms));

function fixture(options: Partial<DaemonLinkOptions> = {}) {
  const clock = new FakeClock();
  const sockets: FakeSocket[] = [];
  let nextId = 0;
  const client = new WakuClient({
    address: "127.0.0.1:4312",
    token: "secret",
    randomUUID: () => `00000000-0000-4000-8000-${String(++nextId).padStart(12, "0")}`,
    now: () => clock.now,
    webSocketFactory: () => {
      const socket = new FakeSocket();
      sockets.push(socket);
      return socket;
    },
  });
  const link = new DaemonLink({
    client,
    active: true,
    now: () => clock.now,
    random: () => 0.5,
    setTimer: clock.set,
    clearTimer: clock.clear,
    ...options,
  });
  const phases: string[] = [];
  link.subscribe((snapshot) => {
    if (phases.at(-1) !== snapshot.phase) phases.push(snapshot.phase);
  });
  return { clock, sockets, client, link, phases };
}

function handshake(socket: FakeSocket): void {
  socket.open();
  socket.receive({ type: "hello", protocolVersion: PROTOCOL_VERSION, daemonVersion: "test" });
}

describe("DaemonLink", () => {
  test("retries at once when a live connection drops, then backs off", async () => {
    const { clock, sockets, link, phases } = fixture();
    const opened = link.open();
    handshake(sockets[0]!);
    expect(await opened).toBe(true);
    expect(link.state).toMatchObject({ phase: "connected", connections: 1, outage: null });

    sockets[0]!.fail("Software caused connection abort");
    await settle();
    expect(link.state.phase).toBe("reconnecting");
    expect(link.state.outage).toMatchObject({
      attempts: 0,
      interrupted: true,
      nextRetryAt: null,
      reason: "Software caused connection abort",
      since: clock.now,
    });
    // The new socket opens after the client's own close handling unwinds.
    expect(sockets).toHaveLength(1);
    await clock.advance(0);
    expect(sockets).toHaveLength(2);

    sockets[1]!.fail("Connection refused");
    await settle();
    expect(link.state.outage).toMatchObject({
      attempts: 1,
      nextRetryAt: clock.now + 750,
      reason: "Connection refused",
    });
    await clock.advance(749);
    expect(sockets).toHaveLength(2);
    await clock.advance(1);
    expect(sockets).toHaveLength(3);
    expect(link.state.outage?.nextRetryAt).toBeNull();

    sockets[2]!.fail("Connection refused");
    await settle();
    expect(link.state.outage).toMatchObject({ attempts: 2, nextRetryAt: clock.now + 1_500 });
    await clock.advance(1_500);
    handshake(sockets[3]!);
    await settle();
    expect(link.state).toMatchObject({ phase: "connected", connections: 2, outage: null });
    expect(phases).toEqual(["connecting", "connected", "reconnecting", "connected"]);
  });

  test("a first connection that fails keeps trying without claiming an interruption", async () => {
    const { clock, sockets, link } = fixture();
    const opened = link.open();
    sockets[0]!.fail("Connection refused");
    expect(await opened).toBe(false);
    expect(link.state).toMatchObject({
      phase: "reconnecting",
      error: null,
      outage: { attempts: 1, interrupted: false, reason: "Connection refused" },
    });
    expect(link.state.outage?.nextRetryAt).toBe(clock.now + 750);
    await clock.advance(750);
    expect(sockets).toHaveLength(2);
  });

  test("stops for failures only the user can fix", async () => {
    const { clock, sockets, link } = fixture();
    const opened = link.open();
    sockets[0]!.open();
    sockets[0]!.receive({ type: "rejected", message: "authentication failed" });
    expect(await opened).toBe(false);
    expect(link.state.phase).toBe("error");
    expect(link.state.error).toContain("rejected this token");
    expect(link.state.outage).toBeNull();
    expect(clock.pending).toEqual([]);
  });

  test("a manual retry skips the wait and restarts the backoff", async () => {
    const { clock, sockets, link } = fixture();
    const opened = link.open();
    sockets[0]!.fail("Connection refused");
    await opened;
    await clock.advance(750);
    sockets[1]!.fail("Connection refused");
    await settle();
    await clock.advance(1_500);
    sockets[2]!.fail("Connection refused");
    await settle();
    expect(link.state.outage).toMatchObject({ attempts: 3, nextRetryAt: clock.now + 3_000 });

    const retried = link.retryNow();
    expect(sockets).toHaveLength(4);
    expect(clock.pending).toEqual([]);
    sockets[3]!.fail("Connection refused");
    expect(await retried).toBe(false);
    expect(link.state.outage).toMatchObject({ attempts: 1, nextRetryAt: clock.now + 750 });
  });

  test("a manual retry during an attempt shares that attempt", async () => {
    const { sockets, link } = fixture();
    const opened = link.open();
    const again = link.retryNow();
    expect(sockets).toHaveLength(1);
    handshake(sockets[0]!);
    expect(await opened).toBe(true);
    expect(await again).toBe(true);
    expect(link.state.connections).toBe(1);
  });

  test("pauses retries in the background and resumes with an immediate attempt", async () => {
    const { clock, sockets, link } = fixture();
    const opened = link.open();
    sockets[0]!.fail("Connection refused");
    await opened;
    expect(clock.pending).toEqual([750]);

    link.setActive(false);
    expect(clock.pending).toEqual([]);
    expect(link.state.outage?.nextRetryAt).toBeNull();
    await clock.advance(60_000);
    expect(sockets).toHaveLength(1);

    link.setActive(true);
    expect(sockets).toHaveLength(2);
    expect(link.state.outage).toMatchObject({ attempts: 1, nextRetryAt: null });
  });

  test("a drop in the background waits for the foreground", async () => {
    const { clock, sockets, link } = fixture();
    const opened = link.open();
    handshake(sockets[0]!);
    await opened;
    link.setActive(false);
    sockets[0]!.fail("Software caused connection abort");
    await settle();
    expect(link.state.phase).toBe("reconnecting");
    expect(clock.pending).toEqual([]);
    link.setActive(true);
    expect(sockets).toHaveLength(2);
  });

  test("probes a quiet connection when the app returns and drops one that stays silent", async () => {
    const { clock, sockets, link } = fixture({ probeTimeoutMs: 5, freshnessMs: 5_000 });
    const opened = link.open();
    handshake(sockets[0]!);
    await opened;

    link.setActive(false);
    clock.now += 10_000;
    link.setActive(true);
    const request = JSON.parse(sockets[0]!.sent.at(-1)!) as { command: unknown };
    expect(request.command).toEqual({ type: "getSettings" });

    await realWait(25);
    expect(link.state.phase).toBe("reconnecting");
    expect(link.state.outage).toMatchObject({
      attempts: 0,
      interrupted: true,
      reason: "The daemon stopped responding.",
    });
    expect(sockets[0]!.readyState).toBe(3);
    await clock.advance(0);
    expect(sockets).toHaveLength(2);
  });

  test("a recent message makes the foreground probe unnecessary", async () => {
    const { clock, sockets, link } = fixture({ freshnessMs: 5_000 });
    const opened = link.open();
    handshake(sockets[0]!);
    await opened;
    link.setActive(false);
    clock.now += 1_000;
    link.setActive(true);
    expect(sockets[0]!.sent).toHaveLength(1);
    expect(link.state.phase).toBe("connected");
  });

  test("an answered probe keeps the connection", async () => {
    const { clock, sockets, link } = fixture({ probeTimeoutMs: 50, freshnessMs: 5_000 });
    const opened = link.open();
    handshake(sockets[0]!);
    await opened;
    link.setActive(false);
    clock.now += 10_000;
    link.setActive(true);
    sockets[0]!.answerLast();
    await realWait(60);
    expect(link.state.phase).toBe("connected");
    expect(sockets).toHaveLength(1);
  });

  test("heartbeats only after the connection has been idle", async () => {
    const { clock, sockets, link } = fixture({ heartbeatIntervalMs: 1_000 });
    const opened = link.open();
    handshake(sockets[0]!);
    await opened;
    expect(clock.pending).toEqual([1_000]);

    await clock.advance(999);
    sockets[0]!.receive({ type: "taskStateChanged", revision: 1 });
    await clock.advance(1);
    // Fresh traffic pushed the probe out rather than sending one.
    expect(sockets[0]!.sent).toHaveLength(1);
    expect(clock.pending).toEqual([999]);

    await clock.advance(999);
    expect(sockets[0]!.sent).toHaveLength(2);
    expect(JSON.parse(sockets[0]!.sent[1]!).command).toEqual({ type: "getSettings" });
    sockets[0]!.answerLast();
    await settle();
    expect(link.state.phase).toBe("connected");
    expect(clock.pending).toEqual([1_000]);
  });

  test("close stops the retry loop and disconnects the client", async () => {
    const { clock, sockets, link, client } = fixture();
    const opened = link.open();
    sockets[0]!.fail("Connection refused");
    await opened;
    expect(clock.pending).toEqual([750]);

    link.close();
    expect(link.state.phase).toBe("closed");
    expect(client.connectionState).toBe("disconnected");
    expect(clock.pending).toEqual([]);
    await clock.advance(60_000);
    expect(sockets).toHaveLength(1);
    expect(await link.retryNow()).toBe(false);
  });
});

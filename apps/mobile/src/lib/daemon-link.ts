import { WakuClient, WakuRpcError, type WakuConnectionState } from '@waku/client';

import { describeConnectionFailure, describeDisconnect, reconnectDelayMs } from './daemon-retry';

export type DaemonLinkPhase = 'connecting' | 'connected' | 'reconnecting' | 'error' | 'closed';

export interface DaemonOutage {
  /** Failed attempts since the outage began. */
  attempts: number;
  /** When the next automatic attempt fires; null while one is in flight or
   * while retries are paused because the app left the foreground. */
  nextRetryAt: number | null;
  /** Why the last attempt failed, or why the connection dropped. */
  reason: string | null;
  /** True when a live connection dropped; false when this daemon has not been
   * reached yet. */
  interrupted: boolean;
  /** When the outage began. */
  since: number;
}

export interface DaemonLinkSnapshot {
  phase: DaemonLinkPhase;
  /** Why the connection needs the user; only in the `error` phase. */
  error: string | null;
  /** The automatic retry loop's progress; only in the `reconnecting` phase. */
  outage: DaemonOutage | null;
  /** Successful connections so far. Past the first, the daemon may have
   * changed while the link was down. */
  connections: number;
}

type TimerHandle = unknown;

export interface DaemonLinkOptions {
  client: WakuClient;
  /** Whether the app is in the foreground. Retries and heartbeats only run there. */
  active?: boolean;
  now?: () => number;
  random?: () => number;
  setTimer?: (callback: () => void, delayMs: number) => TimerHandle;
  clearTimer?: (handle: TimerHandle) => void;
  /** Silence past this on a liveness probe means the socket is dead. */
  probeTimeoutMs?: number;
  /** Silence on an idle connection that triggers a probe. */
  heartbeatIntervalMs?: number;
  /** A message this recent makes a foreground probe unnecessary. */
  freshnessMs?: number;
}

export const DEFAULT_PROBE_TIMEOUT_MS = 8_000;
export const DEFAULT_HEARTBEAT_INTERVAL_MS = 30_000;
export const DEFAULT_FRESHNESS_MS = 5_000;

/**
 * Supervises one client's connection for as long as its daemon stays
 * selected. A dropped connection is retried at once, then with growing
 * jittered waits for as long as the app is in the foreground; a failure only
 * the user can fix (a rejected token, a version mismatch) stops the loop and
 * reports why. The daemon never pings, so an idle connection is probed
 * periodically and whenever the app returns to the foreground: iOS suspends
 * sockets in the background and a network hop can leave one half-open, and
 * without a probe such a socket would only be discovered by a request that
 * hangs. The client keeps its replay cursors and subscriptions across
 * attempts, so a reconnect resumes every followed runtime where it left off.
 */
export class DaemonLink {
  readonly client: WakuClient;

  private snapshot: DaemonLinkSnapshot = {
    phase: 'connecting',
    error: null,
    outage: null,
    connections: 0,
  };
  private readonly listeners = new Set<(snapshot: DaemonLinkSnapshot) => void>();
  private readonly now: () => number;
  private readonly random: () => number;
  private readonly setTimer: (callback: () => void, delayMs: number) => TimerHandle;
  private readonly clearTimer: (handle: TimerHandle) => void;
  private readonly probeTimeoutMs: number;
  private readonly heartbeatIntervalMs: number;
  private readonly freshnessMs: number;
  private readonly unsubscribeClient: () => void;
  private active: boolean;
  private retryTimer: TimerHandle | null = null;
  private heartbeatTimer: TimerHandle | null = null;
  private attempt: Promise<boolean> | null = null;
  private probe: Promise<boolean> | null = null;
  /** True while the client reports an established connection. */
  private live = false;
  private closed = false;

  constructor(options: DaemonLinkOptions) {
    this.client = options.client;
    this.active = options.active ?? true;
    this.now = options.now ?? Date.now;
    this.random = options.random ?? Math.random;
    this.setTimer = options.setTimer ?? ((callback, delayMs) => setTimeout(callback, delayMs));
    this.clearTimer = options.clearTimer
      ?? ((handle) => clearTimeout(handle as ReturnType<typeof setTimeout>));
    this.probeTimeoutMs = options.probeTimeoutMs ?? DEFAULT_PROBE_TIMEOUT_MS;
    this.heartbeatIntervalMs = options.heartbeatIntervalMs ?? DEFAULT_HEARTBEAT_INTERVAL_MS;
    this.freshnessMs = options.freshnessMs ?? DEFAULT_FRESHNESS_MS;
    this.unsubscribeClient = this.client.subscribeConnectionState((state) => {
      this.onClientState(state);
    });
  }

  get state(): DaemonLinkSnapshot {
    return this.snapshot;
  }

  subscribe(listener: (snapshot: DaemonLinkSnapshot) => void): () => void {
    this.listeners.add(listener);
    listener(this.snapshot);
    return () => {
      this.listeners.delete(listener);
    };
  }

  /** The first connection. Resolves false when it failed, whether or not the
   * link went on retrying. */
  open(): Promise<boolean> {
    return this.connect();
  }

  /** A user-initiated retry: skips the wait and restarts the backoff. Shares
   * the attempt already in flight, if any. */
  retryNow(): Promise<boolean> {
    if (this.closed) return Promise.resolve(false);
    if (this.snapshot.phase === 'connected') return Promise.resolve(true);
    if (this.attempt) return this.attempt;
    if (this.snapshot.outage) {
      this.update({ outage: { ...this.snapshot.outage, attempts: 0 } });
    }
    return this.connect();
  }

  /** Foreground changes: pause the timers while backgrounded, and on return
   * retry a pending outage at once or probe a connection that went quiet. */
  setActive(active: boolean): void {
    if (this.closed || this.active === active) return;
    this.active = active;
    if (!active) {
      this.cancelRetry();
      this.cancelHeartbeat();
      const outage = this.snapshot.outage;
      if (outage && outage.nextRetryAt !== null) {
        this.update({ outage: { ...outage, nextRetryAt: null } });
      }
      return;
    }
    if (this.snapshot.phase === 'reconnecting') {
      if (!this.attempt) void this.connect();
      return;
    }
    if (this.snapshot.phase === 'connected') {
      this.scheduleHeartbeat();
      if (this.now() - this.client.lastMessageAt >= this.freshnessMs) {
        void this.probeLiveness();
      }
    }
  }

  /** Asks the daemon for something trivial. Silence past the probe timeout
   * means the socket is half-open, so it is dropped for the retry loop to
   * rebuild. Resolves true when the daemon answered at all. */
  probeLiveness(): Promise<boolean> {
    if (this.closed || this.snapshot.phase !== 'connected') return Promise.resolve(false);
    if (this.probe) return this.probe;
    const probe = (async () => {
      try {
        await this.client.request({ type: 'getSettings' }, undefined, undefined, {
          timeoutMs: this.probeTimeoutMs,
        });
        return true;
      } catch (cause) {
        // An error reply still proves the daemon is there.
        if (cause instanceof WakuRpcError) return true;
        if (this.closed || this.snapshot.phase !== 'connected') return false;
        this.live = false;
        this.client.disconnect();
        this.drop('The daemon stopped responding.');
        return false;
      }
    })();
    this.probe = probe;
    void probe.finally(() => {
      if (this.probe === probe) this.probe = null;
    });
    return probe;
  }

  /** Tears the link down for good; the client disconnects with it. */
  close(): void {
    if (this.closed) return;
    this.closed = true;
    this.cancelRetry();
    this.cancelHeartbeat();
    this.unsubscribeClient();
    this.live = false;
    this.client.disconnect();
    this.update({ phase: 'closed', outage: null });
    this.listeners.clear();
  }

  private connect(): Promise<boolean> {
    if (this.closed) return Promise.resolve(false);
    if (this.attempt) return this.attempt;
    this.cancelRetry();
    const attempt = this.runAttempt();
    this.attempt = attempt;
    void attempt.finally(() => {
      if (this.attempt === attempt) this.attempt = null;
    });
    return attempt;
  }

  private async runAttempt(): Promise<boolean> {
    const outage = this.snapshot.outage;
    this.update(
      outage
        ? { phase: 'reconnecting', outage: { ...outage, nextRetryAt: null } }
        : { phase: 'connecting' },
    );
    if (this.client.connectionState === 'connected') {
      this.onConnected();
      return true;
    }
    try {
      await this.client.connect();
    } catch (cause) {
      if (this.closed) return false;
      this.onFailure(cause);
      return false;
    }
    if (this.closed) return false;
    this.onConnected();
    return true;
  }

  private onConnected(): void {
    this.live = true;
    this.update({
      phase: 'connected',
      error: null,
      outage: null,
      connections: this.snapshot.connections + 1,
    });
    this.scheduleHeartbeat();
  }

  private onFailure(cause: unknown): void {
    const failure = describeConnectionFailure(cause, 'Couldn’t connect to this daemon.');
    if (!failure.retryable) {
      this.update({ phase: 'error', error: failure.message, outage: null });
      return;
    }
    const previous = this.snapshot.outage;
    this.update({
      phase: 'reconnecting',
      error: null,
      outage: {
        attempts: (previous?.attempts ?? 0) + 1,
        nextRetryAt: null,
        reason: failure.message,
        interrupted: previous?.interrupted ?? false,
        since: previous?.since ?? this.now(),
      },
    });
    this.scheduleRetry();
  }

  private onClientState(state: WakuConnectionState): void {
    if (this.closed) return;
    if (state === 'connected') {
      this.live = true;
      return;
    }
    if (state === 'disconnected' && this.live) {
      this.live = false;
      this.drop(describeDisconnect(this.client.lastDisconnectReason));
    }
  }

  /** An established connection is gone: start an outage and retry at once. */
  private drop(reason: string): void {
    if (this.closed || this.snapshot.phase !== 'connected') return;
    this.cancelHeartbeat();
    this.update({
      phase: 'reconnecting',
      error: null,
      outage: {
        attempts: 0,
        nextRetryAt: null,
        reason,
        interrupted: true,
        since: this.now(),
      },
    });
    if (!this.active) return;
    // Leave the client's own notification stack before opening a new socket.
    this.retryTimer = this.setTimer(() => {
      this.retryTimer = null;
      void this.connect();
    }, 0);
  }

  private scheduleRetry(): void {
    const outage = this.snapshot.outage;
    if (!outage || !this.active || this.closed || this.retryTimer !== null) return;
    const delay = reconnectDelayMs(outage.attempts, this.random);
    this.update({ outage: { ...outage, nextRetryAt: this.now() + delay } });
    this.retryTimer = this.setTimer(() => {
      this.retryTimer = null;
      void this.connect();
    }, delay);
  }

  private cancelRetry(): void {
    if (this.retryTimer === null) return;
    this.clearTimer(this.retryTimer);
    this.retryTimer = null;
  }

  private scheduleHeartbeat(): void {
    this.cancelHeartbeat();
    if (this.closed || !this.active || this.snapshot.phase !== 'connected') return;
    const idle = this.now() - this.client.lastMessageAt;
    this.heartbeatTimer = this.setTimer(() => {
      this.heartbeatTimer = null;
      void this.heartbeat();
    }, Math.max(0, this.heartbeatIntervalMs - idle));
  }

  private cancelHeartbeat(): void {
    if (this.heartbeatTimer === null) return;
    this.clearTimer(this.heartbeatTimer);
    this.heartbeatTimer = null;
  }

  private async heartbeat(): Promise<void> {
    if (this.closed || !this.active || this.snapshot.phase !== 'connected') return;
    if (this.now() - this.client.lastMessageAt < this.heartbeatIntervalMs) {
      this.scheduleHeartbeat();
      return;
    }
    await this.probeLiveness();
    this.scheduleHeartbeat();
  }

  private update(patch: Partial<DaemonLinkSnapshot>): void {
    this.snapshot = { ...this.snapshot, ...patch };
    for (const listener of this.listeners) listener(this.snapshot);
  }
}

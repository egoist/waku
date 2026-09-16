import { describe, expect, test } from "bun:test";
import { WakuConnectionError } from "@waku/client";

import {
  RECONNECT_BASE_DELAY_MS,
  RECONNECT_MAX_DELAY_MS,
  describeConnectionFailure,
  describeDisconnect,
  reconnectDelayMs,
} from "./daemon-retry";

describe("reconnectDelayMs", () => {
  test("doubles each attempt within the jitter window", () => {
    expect(reconnectDelayMs(1, () => 0)).toBe(RECONNECT_BASE_DELAY_MS / 2);
    expect(reconnectDelayMs(1, () => 1)).toBe(RECONNECT_BASE_DELAY_MS);
    expect(reconnectDelayMs(2, () => 0)).toBe(1_000);
    expect(reconnectDelayMs(2, () => 1)).toBe(2_000);
    expect(reconnectDelayMs(3, () => 0.5)).toBe(3_000);
    expect(reconnectDelayMs(4, () => 0)).toBe(4_000);
  });

  test("caps the window at the maximum delay", () => {
    expect(reconnectDelayMs(6, () => 0)).toBe(RECONNECT_MAX_DELAY_MS / 2);
    expect(reconnectDelayMs(6, () => 1)).toBe(RECONNECT_MAX_DELAY_MS);
    expect(reconnectDelayMs(40, () => 1)).toBe(RECONNECT_MAX_DELAY_MS);
  });

  test("never waits less than half the window, even with a zero attempt count", () => {
    for (let attempt = 0; attempt < 12; attempt += 1) {
      const delay = reconnectDelayMs(attempt);
      expect(delay).toBeGreaterThanOrEqual(RECONNECT_BASE_DELAY_MS / 2);
      expect(delay).toBeLessThanOrEqual(RECONNECT_MAX_DELAY_MS);
    }
  });
});

describe("describeConnectionFailure", () => {
  test("keeps trying through network faults", () => {
    const refused = describeConnectionFailure(
      new WakuConnectionError(
        "unreachable",
        "The operation couldn’t be completed. Connection refused",
      ),
      "fallback",
    );
    expect(refused).toEqual({ message: "Connection refused", retryable: true });
    expect(describeConnectionFailure(new WakuConnectionError("timeout", "x"), "f").retryable).toBe(true);
    expect(describeConnectionFailure(new WakuConnectionError("closed", "x"), "f").retryable).toBe(true);
  });

  test("stops for failures only the user can fix", () => {
    const rejected = describeConnectionFailure(
      new WakuConnectionError("rejected", "daemon rejected connection: authentication failed"),
      "fallback",
    );
    expect(rejected.retryable).toBe(false);
    expect(rejected.message).toContain("rejected this token");
    expect(describeConnectionFailure(new WakuConnectionError("protocol", "x"), "f").retryable).toBe(false);
    expect(describeConnectionFailure(new WakuConnectionError("handshake", "x"), "f").retryable).toBe(false);
    expect(describeConnectionFailure(new Error("keychain locked"), "fallback")).toEqual({
      message: "keychain locked",
      retryable: false,
    });
    expect(describeConnectionFailure("boom", "fallback")).toEqual({
      message: "fallback",
      retryable: false,
    });
  });
});

describe("describeDisconnect", () => {
  test("keeps the peer's reason and falls back to neutral copy", () => {
    expect(describeDisconnect("Software caused connection abort")).toBe(
      "Software caused connection abort",
    );
    expect(describeDisconnect("The operation couldn’t be completed. Network is down")).toBe(
      "Network is down",
    );
    expect(describeDisconnect(null)).toBe("The daemon connection closed.");
    expect(describeDisconnect("   ")).toBe("The daemon connection closed.");
  });
});

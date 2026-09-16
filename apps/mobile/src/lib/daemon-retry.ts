import { WakuConnectionError } from '@waku/client';

/** First wait after a failed attempt; every later wait doubles. */
export const RECONNECT_BASE_DELAY_MS = 1_000;
/** Longest wait between attempts once the backoff has grown. */
export const RECONNECT_MAX_DELAY_MS = 30_000;

/**
 * How long to wait before the next attempt after `failedAttempts` consecutive
 * failures: exponential growth with equal jitter, so the wait lands in the
 * upper half of the window. The first waits are well under a second, which
 * hides a blip; the steady state settles between 15 and 30 seconds, which
 * keeps a sleeping host from being hammered.
 */
export function reconnectDelayMs(
  failedAttempts: number,
  random: () => number = Math.random,
): number {
  const exponent = Math.max(0, failedAttempts - 1);
  const ceiling = Math.min(RECONNECT_MAX_DELAY_MS, RECONNECT_BASE_DELAY_MS * 2 ** exponent);
  const floor = ceiling / 2;
  const fraction = Math.min(1, Math.max(0, random()));
  return Math.round(floor + fraction * (ceiling - floor));
}

export interface ConnectionFailure {
  /** What to tell the user. */
  message: string;
  /** True when another attempt could succeed without the user changing anything. */
  retryable: boolean;
}

/** Turns a failed connect (or a dropped connection) into user-facing copy and
 * a retry decision. Only the client's own failures are retryable: storage
 * faults, missing tokens, and unknown errors need a person. */
export function describeConnectionFailure(cause: unknown, fallback: string): ConnectionFailure {
  if (cause instanceof WakuConnectionError) {
    switch (cause.kind) {
      case 'rejected':
        return {
          message:
            'The daemon rejected this token. Edit the connection and paste the current token from Waku’s Daemon settings.',
          retryable: false,
        };
      case 'protocol':
        return {
          message:
            'This daemon runs a different Waku version than the app. Update Waku on the host, or update this app.',
          retryable: false,
        };
      case 'handshake':
        return {
          message: 'Something other than a Waku daemon answered at this address. Check the address and port.',
          retryable: false,
        };
      case 'timeout':
        return { message: 'The daemon didn’t answer in time.', retryable: true };
      case 'closed':
        return { message: 'The daemon connection closed.', retryable: true };
      case 'unreachable':
        return { message: cleanNativeReason(cause.message) ?? fallback, retryable: true };
      case 'aborted':
        return { message: cause.message, retryable: false };
    }
  }
  if (cause instanceof Error && cause.message.trim()) {
    return { message: cause.message, retryable: false };
  }
  return { message: fallback, retryable: false };
}

/** Copy for a live connection that ended: the peer's close reason when it
 * gave one, otherwise a neutral line. */
export function describeDisconnect(reason: string | null | undefined): string {
  return (reason && cleanNativeReason(reason)) || 'The daemon connection closed.';
}

/** Native socket reasons read like "The operation couldn’t be completed.
 * Connection refused" — keep the part that names the cause. */
function cleanNativeReason(message: string): string | null {
  const trimmed = message.replace(/^The operation couldn[’']t be completed\.\s*/u, '').trim();
  return trimmed ? trimmed : null;
}

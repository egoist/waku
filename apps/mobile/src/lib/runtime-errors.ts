const DAEMON_DISCONNECT_MESSAGES = new Set([
  'Waku daemon disconnected',
  'Waku daemon is disconnected',
  'Waku client disconnected',
]);

/** Connection loss is owned by the global reconnect banner. It must not
 * survive as a task-level error once that connection has recovered. */
export function isDaemonDisconnectError(cause: unknown): boolean {
  const message = cause instanceof Error ? cause.message : cause;
  return typeof message === 'string' && DAEMON_DISCONNECT_MESSAGES.has(message.trim());
}

/** Drops only stale transport errors and preserves the input reference when
 * there is nothing to remove, so reconnects without errors do not re-render. */
export function clearDaemonDisconnectErrors(
  errors: Record<string, string | undefined>,
): Record<string, string | undefined> {
  let next: Record<string, string | undefined> | null = null;
  for (const [sessionId, error] of Object.entries(errors)) {
    if (!isDaemonDisconnectError(error)) continue;
    next ??= { ...errors };
    delete next[sessionId];
  }
  return next ?? errors;
}

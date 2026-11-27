import type { SessionStatus } from './generated'

/**
 * Whether a session is busy. A failed session is not busy: the desktop lets
 * the user pick another model to retry, because no turn is running.
 */
export function sessionStatusIsBusy(status: SessionStatus): boolean {
  return status === 'connecting'
    || status === 'working'
    || status === 'waiting'
    || status === 'background'
}

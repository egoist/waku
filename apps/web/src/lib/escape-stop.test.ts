import { describe, expect, test } from 'bun:test'
import {
  ESCAPE_STOP_CONFIRMATION_MS,
  isEscapeStopArmed,
  pressEscapeStop,
  sameEscapeStopArm,
} from './escape-stop'

describe('desktop Escape-to-stop behavior', () => {
  test('requires a matching second press within three seconds', () => {
    const first = pressEscapeStop(null, 'session:turn', 1_000)
    expect(first.type).toBe('arm')
    if (first.type !== 'arm') throw new Error('expected an armed stop')

    expect(ESCAPE_STOP_CONFIRMATION_MS).toBe(3_000)
    expect(isEscapeStopArmed(first.arm, 'session:turn', 3_999)).toBe(true)
    expect(pressEscapeStop(first.arm, 'session:turn', 3_999)).toEqual({ type: 'stop' })
  })

  test('rearms after expiry or when the active turn changes', () => {
    const first = pressEscapeStop(null, 'session:turn-1', 1_000)
    if (first.type !== 'arm') throw new Error('expected an armed stop')

    const expired = pressEscapeStop(first.arm, 'session:turn-1', 4_000)
    expect(expired.type).toBe('arm')
    const replaced = pressEscapeStop(first.arm, 'session:turn-2', 2_000)
    expect(replaced.type).toBe('arm')
  })

  test('compares timer arms by value', () => {
    const arm = { target: 'session:turn', expiresAt: 4_000 }
    expect(sameEscapeStopArm({ ...arm }, arm)).toBe(true)
    expect(sameEscapeStopArm({ ...arm, expiresAt: 4_001 }, arm)).toBe(false)
  })
})

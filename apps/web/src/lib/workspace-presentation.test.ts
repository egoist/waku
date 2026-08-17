import { describe, expect, test } from 'bun:test'
import { shouldShowInitialDestination } from './workspace-presentation'

describe('shouldShowInitialDestination', () => {
  test('keeps startup visible while the first destination is unresolved', () => {
    expect(shouldShowInitialDestination(false, {
      choosing: false,
      restoringNewTask: false,
      hydratingSession: true,
    })).toBe(true)
  })

  test('never returns to startup for a session switch after New Task was shown', () => {
    expect(shouldShowInitialDestination(true, {
      choosing: false,
      restoringNewTask: false,
      hydratingSession: true,
    })).toBe(false)
  })

  test('does not block a resolved first destination', () => {
    expect(shouldShowInitialDestination(false, {
      choosing: false,
      restoringNewTask: false,
      hydratingSession: false,
    })).toBe(false)
  })
})

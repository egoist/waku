import { describe, expect, test } from 'bun:test'
import {
  readRememberedNavigation,
  routeDestinationTransition,
  taskRemovalDestination,
  writeRememberedNavigation,
} from './navigation-memory'
import { createProject, createSession } from './daemon-api'

describe('remembered navigation', () => {
  test('restores a new task and its project per daemon', () => {
    const storage = memoryStorage()
    writeRememberedNavigation(storage, 'ws://first', {
      kind: 'newTask',
      projectId: 'waku',
    })
    writeRememberedNavigation(storage, 'ws://second', {
      kind: 'session',
      sessionId: 'session-2',
    })

    expect(readRememberedNavigation(storage, 'ws://first')).toEqual({
      kind: 'newTask',
      projectId: 'waku',
    })
    expect(readRememberedNavigation(storage, 'ws://second')).toEqual({
      kind: 'session',
      sessionId: 'session-2',
    })
  })

  test('ignores malformed state', () => {
    const storage = memoryStorage('{broken')
    expect(readRememberedNavigation(storage, 'ws://first')).toBeNull()
  })
})

describe('browser route transitions', () => {
  test('Back from New Task activates the session in the URL', () => {
    expect(routeDestinationTransition(undefined, 'session-1', true)).toBe('session')
  })

  test('Forward from a session restores the New Task route', () => {
    expect(routeDestinationTransition('session-1', undefined, false)).toBe('newTask')
  })

  test('an explicit New Task transition does not reinitialize its draft', () => {
    expect(routeDestinationTransition('session-1', undefined, true)).toBeNull()
  })
})

describe('selected task removal', () => {
  test('opens the newest remaining task in the same project', () => {
    const project = { ...createProject('/repos/waku'), id: 'waku' }
    const removed = { ...createSession(project.id, 'codex', false), id: 'removed' }
    const older = {
      ...createSession(project.id, 'codex', false),
      id: 'older',
      updated_at: 10,
    }
    const newer = {
      ...createSession(project.id, 'claude', false),
      id: 'newer',
      updated_at: 20,
    }

    expect(taskRemovalDestination(
      [project],
      [project],
      [older, newer],
      removed,
    )).toEqual({ kind: 'session', sessionId: 'newer' })
  })

  test('opens a fresh task in the same ordinary project when history is empty', () => {
    const project = { ...createProject('/repos/waku'), id: 'waku' }
    const removed = createSession(project.id, 'codex', false)
    expect(taskRemovalDestination([project], [project], [], removed)).toEqual({
      kind: 'newTask',
      project,
    })
  })

  test('creates a fresh projectless workspace after deleting its last task', () => {
    const project = {
      ...createProject('/home/user/.waku/projects/old-task'),
      id: 'projectless',
      name: 'No project',
    }
    const removed = createSession(project.id, 'codex', false)
    expect(taskRemovalDestination([project], [], [], removed)).toEqual({
      kind: 'projectless',
    })
  })
})

function memoryStorage(initial?: string) {
  const values = new Map<string, string>()
  if (initial !== undefined) values.set('waku.navigation', initial)
  return {
    getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => { values.set(key, value) },
  }
}

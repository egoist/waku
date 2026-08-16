import { describe, expect, test } from 'bun:test'
import type {
  AgentSession,
  Automation,
  ComposerDraftChange,
  DaemonSettings,
  Project,
  WakuClient,
} from '@waku/client'
import {
  applyAutomationChanges,
  applyComposerDraftChanges,
  beginTurn,
  browseDaemonDirectory,
  captureTurnCheckpoint,
  captureTurnStart,
  createProject,
  createSession,
  persistProject,
  persistSession,
  probeProvider,
  removeSession,
  runAutomation,
  selectableProjects,
  writeWorkspaceTextFile,
  type DaemonDirectory,
} from './daemon-api'

describe('automation daemon commands', () => {
  test('routes run-now through the daemon execution command', async () => {
    let command: unknown
    const automation = {} as Automation
    const session = {} as AgentSession
    const client = {
      request: async (next: unknown) => {
        command = next
        return {
          type: 'automationRunStarted',
          automation,
          session,
          runtimeId: 'runtime',
          supportsSteer: true,
        }
      },
    } as unknown as WakuClient

    await expect(runAutomation(client, 'automation')).resolves.toMatchObject({
      automation,
      session,
      runtimeId: 'runtime',
      supportsSteer: true,
    })
    expect(command).toEqual({
      type: 'runAutomation',
      automationId: 'automation',
      catchUp: false,
    })
  })

  test('sends an upsert as one targeted automation delta payload', async () => {
    let command: unknown
    const automation = { id: 'automation' } as Automation
    const changes = [{ kind: 'upsert', automation }] as const
    const client = {
      request: async (next: unknown) => {
        command = next
        return { type: 'automationChangesApplied', automations: [automation] }
      },
    } as unknown as WakuClient

    await expect(applyAutomationChanges(client, [...changes])).resolves.toEqual([automation])
    expect(command).toEqual({ type: 'applyAutomationChanges', changes: [...changes] })
  })

  test('puts the explicit cascade decision in a remove delta payload', async () => {
    let command: unknown
    const changes = [{
      kind: 'remove',
      automation_id: 'automation',
      cascade_sessions: false,
    }] as const
    const client = {
      request: async (next: unknown) => {
        command = next
        return { type: 'automationChangesApplied', automations: [] }
      },
    } as unknown as WakuClient

    await expect(applyAutomationChanges(client, [...changes])).resolves.toEqual([])
    expect(command).toEqual({ type: 'applyAutomationChanges', changes: [...changes] })
  })
})

describe('applyComposerDraftChanges', () => {
  test('sends keyed updates instead of replacing every client draft', async () => {
    let command: unknown
    const client = {
      request: async (next: unknown) => {
        command = next
        return { type: 'ack' }
      },
    } as unknown as WakuClient
    const changes: ComposerDraftChange[] = [{
      target: { type: 'session', sessionId: 'session' },
      draft: { text: 'keep this', attachments: [] },
    }]

    await expect(applyComposerDraftChanges(client, changes)).resolves.toBeUndefined()
    expect(command).toEqual({ type: 'applyComposerDraftChanges', changes })
  })
})

describe('beginTurn', () => {
  test('puts the submitted prompt in the transcript before runtime startup', () => {
    const draft = createSession('project', 'codex', false)
    const active = beginTurn(draft, 'Build the feature')

    expect(active.status).toBe('connecting')
    expect(active.messages).toHaveLength(1)
    expect(active.messages[0]).toMatchObject({
      role: 'user',
      content: 'Build the feature',
      streaming: false,
    })
    expect(active.turns).toHaveLength(1)
    expect(draft.messages).toHaveLength(0)
  })
})

describe('browseDaemonDirectory', () => {
  test('lists an absolute directory on the daemon host', async () => {
    let command: unknown
    const result: DaemonDirectory = {
      type: 'directory',
      path: '/Users/me',
      parent: '/Users',
      home: '/Users/me',
      filesystem_root: '/',
      entries: [],
    }
    const client = {
      request: async (next: unknown) => {
        command = next
        return { type: 'workspace', result }
      },
    } as unknown as WakuClient

    await expect(browseDaemonDirectory(client, '/Users/me')).resolves.toEqual(result)
    expect(command).toEqual({
      type: 'workspace',
      operation: { type: 'browseDirectory', path: '/Users/me' },
    })
  })

  test('uses the daemon home when no path is provided', async () => {
    let command: unknown
    const result: DaemonDirectory = {
      type: 'directory',
      path: '/Users/me',
      parent: '/Users',
      home: '/Users/me',
      filesystem_root: '/',
      entries: [],
    }
    const client = {
      request: async (next: unknown) => {
        command = next
        return { type: 'workspace', result }
      },
    } as unknown as WakuClient

    await expect(browseDaemonDirectory(client, null)).resolves.toEqual(result)
    expect(command).toEqual({
      type: 'workspace',
      operation: { type: 'browseDirectory', path: null },
    })
  })
})

describe('turn checkpoints', () => {
  test('captures the immutable starting ref on the daemon host', async () => {
    let command: unknown
    const client = {
      request: async (next: unknown) => {
        command = next
        return { type: 'workspace', result: { type: 'ack' } }
      },
    } as unknown as WakuClient

    await expect(captureTurnStart(client, '/srv/waku', 'session', 2)).resolves.toBeUndefined()
    expect(command).toEqual({
      type: 'workspace',
      operation: {
        type: 'captureTurnStart',
        cwd: '/srv/waku',
        session_id: 'session',
        turn_count: 2,
      },
    })
  })

  test('returns the ending checkpoint captured by the daemon', async () => {
    let command: unknown
    const checkpoint = {
      turn_count: 2,
      git_ref: 'refs/waku/session-session-turn-2',
      status: 'ready' as const,
      files: [],
      additions: 0,
      deletions: 0,
      created_at: 1,
    }
    const client = {
      request: async (next: unknown) => {
        command = next
        return { type: 'workspace', result: { type: 'checkpoint', checkpoint } }
      },
    } as unknown as WakuClient

    await expect(captureTurnCheckpoint(client, '/srv/waku', 'session', 2))
      .resolves.toEqual(checkpoint)
    expect(command).toEqual({
      type: 'workspace',
      operation: {
        type: 'captureTurn',
        cwd: '/srv/waku',
        session_id: 'session',
        turn_count: 2,
      },
    })
  })
})

describe('probeProvider', () => {
  test('can detect an executable without starting model or version discovery', async () => {
    let command: unknown
    const client = {
      request: async (next: unknown) => {
        command = next
        return {
          type: 'providerProbe',
          probe: {
            provider: 'codex',
            installed: true,
            path: '/opt/waku/codex',
            models: [],
            agent_presets: [],
          },
          version: null,
        }
      },
    } as unknown as WakuClient
    // Empty override maps are omitted by serde even though the generated
    // TypeScript type currently marks the field as required.
    const settings = {} as DaemonSettings

    await expect(probeProvider(client, 'codex', settings, {
      discoverModels: false,
      probeVersion: false,
    })).resolves.toMatchObject({ installed: true, path: '/opt/waku/codex' })
    expect(command).toEqual({
      type: 'probeProvider',
      provider: 'codex',
      binaryOverride: null,
      discoverModels: false,
      probeVersion: false,
    })
  })
})

describe('writeWorkspaceTextFile', () => {
  test('writes the edited contents through the daemon workspace API', async () => {
    let command: unknown
    const client = {
      request: async (next: unknown) => {
        command = next
        return { type: 'workspace', result: { type: 'ack' } }
      },
    } as unknown as WakuClient

    await expect(
      writeWorkspaceTextFile(client, '/srv/waku', 'src/app.ts', 'export const ready = true\n'),
    ).resolves.toBeUndefined()
    expect(command).toEqual({
      type: 'workspace',
      operation: {
        type: 'writeTextFile',
        root: '/srv/waku',
        relative_path: 'src/app.ts',
        content: 'export const ready = true\n',
      },
    })
  })
})

describe('createProject', () => {
  test('normalizes a remote absolute path without collapsing the root', () => {
    expect(createProject('/').path).toBe('/')
    expect(createProject('/srv/waku/').path).toBe('/srv/waku')
    expect(createProject('/srv/waku/').name).toBe('waku')
  })

  test('rejects paths that depend on the browser process cwd', () => {
    expect(() => createProject('relative/project')).toThrow('absolute path')
  })
})

describe('persistProject', () => {
  test('adds a daemon-host project without creating a session', async () => {
    const existing = project('existing', 'existing', '/srv/existing')
    const candidate = project('new', 'waku', '/srv/waku')
    const commands: unknown[] = []
    const client = {
      request: async (command: unknown) => {
        commands.push(command)
        if ((command as { type: string }).type === 'loadTaskState') {
          return {
            type: 'taskState',
            projects: [existing],
            sessions: [{ id: 'session' }],
            defaultCwd: '/srv',
            projectlessRoot: '/srv/.waku/projects',
          }
        }
        return { type: 'taskStateSaved', sessions: [] }
      },
    } as unknown as WakuClient

    const result = await persistProject(client, candidate)

    expect(result.project).toEqual(candidate)
    expect(result.taskState.projects).toEqual([existing, candidate])
    expect(commands).toEqual([
      { type: 'loadTaskState' },
      {
        type: 'saveTaskState',
        projects: [existing, candidate],
        liveSessionIds: ['session'],
        sessions: [],
      },
    ])
  })

  test('reuses a project already persisted for the same daemon path', async () => {
    const existing = project('existing', 'waku', '/srv/waku')
    const commands: unknown[] = []
    const client = {
      request: async (command: unknown) => {
        commands.push(command)
        return {
          type: 'taskState',
          projects: [existing],
          sessions: [],
          defaultCwd: '/srv',
          projectlessRoot: '/srv/.waku/projects',
        }
      },
    } as unknown as WakuClient

    const result = await persistProject(client, project('duplicate', 'waku', '/srv/waku'))

    expect(result.project).toEqual(existing)
    expect(commands).toEqual([{ type: 'loadTaskState' }])
  })
})

describe('persistSession', () => {
  test('checkpoints one session without reloading or replacing the catalog', async () => {
    const saved = createSession('project', 'codex', false)
    const commands: unknown[] = []
    const client = {
      request: async (command: unknown) => {
        commands.push(command)
        return { type: 'taskStateSaved', sessions: [saved] }
      },
    } as unknown as WakuClient

    await expect(persistSession(client, saved)).resolves.toEqual(saved)
    expect(commands).toEqual([{
      type: 'saveTaskState',
      projects: [],
      liveSessionIds: [saved.id],
      sessions: [saved],
    }])
  })
})

describe('selectableProjects', () => {
  test('represents projectless tasks as one choice while preserving the selected workspace', () => {
    const ordinary = project('repo', 'waku', '/srv/waku')
    const first = project('one', 'No project', '/home/me/.waku/projects/one')
    const selected = project('two', 'No project', '/home/me/.waku/projects/two')

    expect(selectableProjects([ordinary, first, selected], selected)).toEqual([
      selected,
      ordinary,
    ])
  })
})

describe('removeSession', () => {
  test('removes only the selected session through the daemon', async () => {
    const commands: unknown[] = []
    const client = {
      request: async (next: unknown) => {
        commands.push(next)
        if ((next as { type: string }).type === 'removeSession') {
          return { type: 'ack' }
        }
        if ((next as { type: string }).type === 'loadTaskState') {
          return {
            type: 'taskState',
            projects: [],
            sessions: [{ id: 'keep' }],
            defaultCwd: '/srv',
            projectlessRoot: '/srv/.waku/projects',
          }
        }
        throw new Error('unexpected command')
      },
    } as unknown as WakuClient

    const next = await removeSession(client, 'remove')

    expect(next.sessions.map((session) => session.id)).toEqual(['keep'])
    expect(commands).toEqual([
      { type: 'removeSession' },
      { type: 'loadTaskState' },
    ])
  })
})

function project(id: string, name: string, path: string): Project {
  return { id, name, path, created_at: 0 }
}

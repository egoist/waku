import { describe, expect, test } from 'bun:test'
import type { AgentSession, Project } from '@waku/client'
import {
  dateGroup,
  formatTimeAgo,
  formatWorkingElapsed,
  groupSessions,
  nextSidebarUpdateDelay,
  sessionHasStarted,
  sessionTimeLabel,
  sidebarRows,
} from './sidebar-presentation'

describe('desktop sidebar presentation', () => {
  test('uses a Monday-based current week instead of a rolling seven days', () => {
    const wednesday = new Date(2026, 7, 12, 12)
    expect(dateGroup(atLocalNoon(2026, 7, 12), wednesday)).toBe('today')
    expect(dateGroup(atLocalNoon(2026, 7, 11), wednesday)).toBe('yesterday')
    expect(dateGroup(atLocalNoon(2026, 7, 10), wednesday)).toBe('week')
    expect(dateGroup(atLocalNoon(2026, 7, 9), wednesday)).toBe('month')
  })

  test('keeps Add Project in an empty history through the first group header', () => {
    expect(sidebarRows([], new Set())).toEqual([
      { kind: 'search', key: 'search' },
      {
        kind: 'group',
        key: 'group:today',
        group: { id: 'today', label: 'Today', sessions: [] },
        collapsed: false,
        first: true,
      },
    ])
  })

  test('matches desktop settled and live time labels', () => {
    expect(formatTimeAgo(0)).toBe('just now')
    expect(formatTimeAgo(604_800)).toBe('7d')
    expect(formatWorkingElapsed(65)).toBe('1m 5s')
    expect(formatWorkingElapsed(3_720)).toBe('1h 2m')

    const live = session({
      status: 'working',
      turns: [{
        id: 'turn',
        turn_count: 1,
        status: 'running',
        provider_turn_started: true,
        started_at: 100,
        completed_at: null,
        checkpoint: null,
      }],
    })
    expect(sessionTimeLabel(live, 165)).toBe('Working for 1m 5s')
    expect(nextSidebarUpdateDelay([live], 165)).toBe(1)
  })

  test('does not invent a reply time and keeps cursor-only resumed tasks', () => {
    const resumed = session({ provider_cursor: { provider: 'codex', value: {} } as never })
    expect(sessionHasStarted(resumed)).toBe(true)
    expect(sessionTimeLabel(resumed, 1_000)).toBeNull()
  })

  test('presents the projectless sentinel with the localized desktop name', () => {
    const project: Project = {
      id: 'project',
      name: 'No project',
      path: '/home/me/.waku/projects/session',
      created_at: 1,
    }
    const groups = groupSessions(
      [project],
      [session({ messages: [{ id: 'message' } as never] })],
      new Date(2026, 7, 15, 12),
      'Unknown project',
      'プロジェクトなし',
    )
    expect(groups[0]?.sessions[0]?.projectName).toBe('プロジェクトなし')
  })
})

function atLocalNoon(year: number, month: number, day: number): number {
  return Math.floor(new Date(year, month, day, 12).getTime() / 1_000)
}

function session(patch: Partial<AgentSession>): AgentSession {
  return {
    id: 'session',
    title: 'New Task',
    project_id: 'project',
    provider: 'codex',
    model: null,
    runtime_mode: 'ask',
    interaction_mode: 'build',
    status: 'idle',
    created_at: 10,
    updated_at: 10,
    provider_cursor: null,
    messages: [],
    transcript_blocks: [],
    turns: [],
    ...patch,
  }
}

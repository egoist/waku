import { describe, expect, test } from 'bun:test'
import type { ActivityItem, AgentSession } from '@waku/client'
import {
  activityActionLabel,
  activityDisclosureSections,
  activityDisplayTitle,
  activityFileChangeStats,
  activityGroupIsLive,
  activityHeaderTitle,
  activityPreview,
  activityRowDetail,
  activitySummary,
  activityTextRows,
  assistantResponseFooters,
  fencedCode,
  reasoningTitle,
  shouldVirtualizeActivityText,
  turnAnswerStart,
  turnFoldLabel,
  userMessageRewindTurnCount,
  formatMessageTime,
  formatWorkingElapsed,
} from './transcript-presentation'

describe('desktop transcript language', () => {
  test('uses the desktop stopped and worked labels', () => {
    expect(turnFoldLabel(turn('interrupted', 100, 164))).toBe('You stopped after 1 minute 4 seconds')
    expect(turnFoldLabel(turn('completed', 100, 160))).toBe('Worked for 1 minute')
    expect(turnFoldLabel(turn('completed', 100, 3_820))).toBe('Worked for 1 hour 2 minutes')
  })

  test('calls provider reasoning a thought', () => {
    expect(reasoningTitle(reasoning(false, 0, 0))).toBe('Thinking')
    expect(reasoningTitle(reasoning(true, 1_000, 65_000))).toBe('Thought for 1 minute 4 seconds')
  })

  test('uses thought nouns in grouped activity summaries', () => {
    expect(activitySummary([
      reasoning(true, 0, 1_000),
      { ...reasoning(true, 0, 1_000), id: 'thought-2' },
      activity('command', true),
    ])).toBe('Ran 2 thoughts · 1 command')
    expect(activitySummary([reasoning(false, 0, 0)])).toBe('Running 1 thought')
  })

  test('summarizes an activity group only after it leaves the live tail', () => {
    const command = {
      ...activity('command', false),
      display_target: 'git log --oneline -15',
    }
    const activities = [reasoning(true, 0, 1_000), command]

    expect(activityHeaderTitle(activities, true)).toBe('Running git log --oneline -15')
    expect(activityGroupIsLive(true, true, 1, 1)).toBe(true)
    command.complete = true
    expect(activityHeaderTitle(activities, true)).toBe('Ran git log --oneline -15')
    expect(activityGroupIsLive(true, true, 1, 2)).toBe(false)
    expect(activityHeaderTitle(activities, false)).toBe('Ran 1 thought · 1 command')
    expect(activityGroupIsLive(true, false, 1, 1)).toBe(false)
    expect(activityGroupIsLive(false, true, 1, 1)).toBe(false)
    expect(activityActionLabel(command)).toBe('Run')
    expect(activityRowDetail(command)).toBe('git log --oneline -15')
  })

  test('keeps generic tool names and labels AskUserQuestion by purpose', () => {
    const named = {
      ...activity('tool', true),
      title: 'mcp__threads__create_thread',
    }
    const question = {
      ...activity('tool', true),
      title: 'AskUserQuestion',
    }
    const unnamed = {
      ...activity('tool', true),
      title: 'Tool',
    }

    expect(activityActionLabel(named)).toBe('Tool')
    expect(activityRowDetail(named)).toBe('Create thread')
    expect(activityDisplayTitle(named)).toBe('Create thread')
    expect(activityActionLabel(unnamed)).toBe('Tool')
    expect(activityRowDetail(unnamed)).toBe('')
    expect(activityActionLabel(question)).toBe('Ask questions')
    expect(activityRowDetail(question)).toBe('')
    expect(activityDisplayTitle(question)).toBe('Ask questions')
  })

  test('derives the same provider-neutral activity titles as desktop', () => {
    expect(activityDisplayTitle({
      ...activity('fileChange', true),
      title: 'Edit file',
      file_changes: [{ path: '/tmp/src/app.ts', additions: 4, deletions: 1 }],
    })).toBe('Edited app.ts')
    expect(activityDisplayTitle({
      ...activity('command', false),
      title: 'Run command',
      display_target: 'bun test',
    })).toBe('Running bun test')
    expect(activityDisplayTitle({
      ...activity('command', true),
      title: 'Run command',
      display_target: 'python3 analyze.py',
      display_description: 'Analyze color statistics',
    })).toBe('Ran command: Analyze color statistics')
    expect(activityDisplayTitle({
      ...activity('command', false),
      title: 'Run command',
      display_target: 'python3 analyze.py',
      display_description: 'Analyze color statistics',
    })).toBe('Running command: Analyze color statistics')
    expect(activityDisplayTitle({
      ...activity('fileRead', true),
      title: 'Read file',
      display_target: '/tmp/README.md',
    })).toBe('Read README.md')
  })

  test('keeps activity arguments and output in separate disclosure sections', () => {
    const item = {
      ...activity('tool', true),
      arguments: '{"cmd":"bun test"}',
      output: '12 pass',
      detail: 'Completed',
    }
    expect(activityDisclosureSections(item)).toEqual([
      { kind: 'arguments', label: 'Arguments', content: '{"cmd":"bun test"}' },
      { kind: 'output', label: 'Output', content: '12 pass' },
    ])
    expect(activityPreview({ ...item, detail: 'failed', output: '\npermission denied\nmore' }))
      .toBe('permission denied')
  })

  test('shows only the normalized command and output for command details', () => {
    const item = {
      ...activity('command', true),
      arguments: 'bun test',
      display_target: 'bun test',
      output: '12 pass',
      detail: 'Completed',
    }
    expect(activityDisclosureSections(item)).toEqual([
      { kind: 'command', label: 'Command', content: 'bun test' },
      { kind: 'output', label: 'Output', content: '12 pass' },
    ])
  })

  test('shows file edit stats only when every settled edit has counts', () => {
    const item = {
      ...activity('fileChange', true),
      file_changes: [
        { path: 'a.ts', additions: 3, deletions: 1 },
        { path: 'b.ts', additions: 2, deletions: 4 },
      ],
    }
    expect(activityFileChangeStats(item)).toEqual({ additions: 5, deletions: 5 })
    expect(activityFileChangeStats({ ...item, file_changes: [{ path: 'a.ts' }] })).toBeNull()
  })

  test('keeps the live working duration compact', () => {
    expect(formatWorkingElapsed(9)).toBe('9s')
    expect(formatWorkingElapsed(65)).toBe('1m 5s')
    expect(formatWorkingElapsed(3_720)).toBe('1h 2m')
  })

  test('adds desktop calendar context to message times', () => {
    const now = new Date(2026, 7, 9, 16, 0)
    expect(formatMessageTime(seconds(2026, 7, 9, 9, 5), now, 'en-US')).toBe('9:05 AM')
    expect(formatMessageTime(seconds(2026, 7, 8, 17, 0), now, 'en-US')).toBe('Yesterday 5:00 PM')
    expect(formatMessageTime(seconds(2026, 7, 7, 13, 12), now, 'en-US')).toBe('Friday 1:12 PM')
    expect(formatMessageTime(seconds(2026, 4, 12, 23, 0), now, 'en-US')).toBe('May 12th, 11:00 PM')
    expect(formatMessageTime(seconds(2024, 7, 4, 11, 0), now, 'en-US')).toBe('Aug 4th 2024, 11:00 AM')
  })

  test('puts one footer on the terminal answer and copies every visible part', () => {
    const session = transcriptSession()
    const footers = assistantResponseFooters(session)
    expect([...footers.keys()]).toEqual([3])
    expect(footers.get(3)).toEqual({
      content: 'First half.\n\nSecond half.',
      timestamp: 200,
    })
  })

  test('copies fenced code with the same language-marker handling as desktop', () => {
    expect(fencedCode('before\n```ts\nconst one = 1\n```\nafter\n```\nplain\n```')).toBe(
      'const one = 1\n\nplain',
    )
    expect(fencedCode('no code here')).toBeNull()
  })

  test('virtualizes huge activity output and bounds individual layout rows', () => {
    const manyLines = Array.from({ length: 201 }, (_, index) => `line ${index}`).join('\n')
    const manyRows = activityTextRows(manyLines)
    expect(manyRows).toHaveLength(201)
    expect(shouldVirtualizeActivityText(manyLines, manyRows)).toBe(true)

    const hugeLine = 'x'.repeat(4_500)
    const hugeRows = activityTextRows(hugeLine)
    expect(hugeRows.map((row) => row.length)).toEqual([2_000, 2_000, 500])
    expect(shouldVirtualizeActivityText(hugeLine, hugeRows)).toBe(false)

    const hugePayload = 'x'.repeat(40_001)
    expect(shouldVirtualizeActivityText(hugePayload, activityTextRows(hugePayload))).toBe(true)
  })

  test('uses the desktop fold boundary when work follows the final text part', () => {
    const rows = ['thought', 'answer one', 'answer two', 'tool']
    expect(turnAnswerStart(rows, (row) => row.startsWith('answer'))).toBe(1)
    expect(turnAnswerStart(['thought', 'tool'], () => false)).toBe(2)
  })

  test('does not publish a footer while the turn is running', () => {
    const session = transcriptSession()
    session.turns[0]!.status = 'running'
    session.turns[0]!.completed_at = null
    expect(assistantResponseFooters(session).size).toBe(0)
  })

  test('offers prior-message rewind only from a daemon checkpoint and resumable provider', () => {
    const session = transcriptSession()
    session.provider_cursor = { provider: 'codex', threadId: 'thread' }
    const firstMessage = session.messages[0]!

    expect(userMessageRewindTurnCount(session, firstMessage, new Set([0]))).toBe(1)
    expect(userMessageRewindTurnCount(session, firstMessage, new Set())).toBeNull()

    session.provider_cursor = null
    expect(userMessageRewindTurnCount(session, firstMessage, new Set([0]))).toBeNull()

    session.provider_cursor = { provider: 'codex', threadId: 'thread' }
    session.status = 'working'
    expect(userMessageRewindTurnCount(session, firstMessage, new Set([0]))).toBeNull()
  })
})

function transcriptSession(): AgentSession {
  return {
    id: 'session',
    title: 'New Task',
    project_id: 'project',
    provider: 'codex',
    runtime_mode: 'fullAccess',
    interaction_mode: 'build',
    status: 'idle',
    created_at: 10,
    updated_at: 200,
    provider_cursor: null,
    turns: [turn('completed', 100, 200)],
    queued_messages: [],
    transcript_blocks: [{
      turn_id: 'turn',
      after_message: 2,
      content: { kind: 'activities', data: [] },
    }],
    messages: [
      message('user', 'Build it', 0),
      message('assistant', 'Interim commentary.', 1),
      message('assistant', 'First half.', 2),
      message('assistant', 'Second half.', 3),
    ],
  }
}

function message(
  role: AgentSession['messages'][number]['role'],
  content: string,
  index: number,
): AgentSession['messages'][number] {
  return {
    id: `message-${index}`,
    turn_id: 'turn',
    role,
    content,
    created_at: 100 + index,
    streaming: false,
  }
}

function seconds(year: number, month: number, day: number, hour: number, minute: number) {
  return new Date(year, month, day, hour, minute).getTime() / 1_000
}

function turn(
  status: AgentSession['turns'][number]['status'],
  startedAt: number,
  completedAt: number,
): AgentSession['turns'][number] {
  return {
    id: 'turn',
    turn_count: 1,
    status,
    provider_turn_started: true,
    started_at: startedAt,
    completed_at: completedAt,
    checkpoint: null,
  }
}

function reasoning(complete: boolean, startedAt: number, finishedAt: number): ActivityItem {
  return {
    ...activity('reasoning', complete),
    reasoning: {
      content: 'Checking',
      started_at_ms: startedAt,
      finished_at_ms: finishedAt,
    },
  }
}

function activity(kind: ActivityItem['kind'], complete: boolean): ActivityItem {
  return {
    id: kind,
    source_id: kind,
    kind,
    title: kind,
    detail: null,
    display_target: null,
    display_description: null,
    output: null,
    failed: false,
    complete,
    image_urls: [],
    reasoning: null,
  }
}

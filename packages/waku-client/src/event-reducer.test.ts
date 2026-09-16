import { describe, expect, test } from 'bun:test'

import { reduceRuntimeEvent } from './event-reducer'
import { activityDisclosureSections } from './transcript-presentation'
import type { AgentSession, SequencedEvent } from './generated'

const clock = {
  nowSeconds: () => 200,
  nowMillis: () => 200_000,
  randomUUID: (() => {
    let id = 0
    return () => `00000000-0000-4000-8000-${String(++id).padStart(12, '0')}`
  })(),
}

const SUBMISSION = {
  message: 'Second prompt',
  turnId: '10000000-0000-4000-8000-000000000002',
  messageId: '20000000-0000-4000-8000-000000000002',
}

describe('promptSubmitted', () => {
  test('a client following the runtime mirrors another client’s submission under its ids', () => {
    // The desktop stayed attached to the idle runtime after the first turn;
    // the phone then submitted the second prompt.
    const result = reduceRuntimeEvent(idleSession(), event('promptSubmitted', SUBMISSION), clock)
    const session = result.session

    expect(session.status).toBe('connecting')
    expect(session.turns).toHaveLength(2)
    expect(session.turns.at(-1)).toMatchObject({
      id: SUBMISSION.turnId,
      turn_count: 2,
      status: 'running',
      provider_turn_started: false,
    })
    expect(session.messages.at(-1)).toMatchObject({
      id: SUBMISSION.messageId,
      turn_id: SUBMISSION.turnId,
      role: 'user',
      content: 'Second prompt',
    })

    // The provider's start confirms that turn instead of inventing one, so
    // the reply streams under the prompt that asked for it.
    const started = apply(session, 'turnStarted', null)
    expect(started.turns).toHaveLength(2)
    expect(started.turns.at(-1)?.provider_turn_started).toBe(true)
    const replied = apply(started, 'textDelta', 'Sure.')
    expect(replied.messages.map((message) => message.role)).toEqual([
      'user',
      'assistant',
      'user',
      'assistant',
    ])
    expect(replied.messages.at(-1)?.turn_id).toBe(SUBMISSION.turnId)
  })

  test('the submitting client’s own echo changes nothing', () => {
    const session = runningSession()
    const result = reduceRuntimeEvent(
      session,
      event('promptSubmitted', {
        message: 'Go',
        turnId: SUBMISSION.turnId,
        messageId: SUBMISSION.messageId,
      }),
      clock,
    )

    expect(result.session.turns).toEqual(session.turns)
    expect(result.session.messages).toEqual(session.messages)
    expect(result.session.status).toBe('connecting')
  })

  test('a provider-started turn without a prompt receives the submitted message', () => {
    const session: AgentSession = {
      ...idleSession(),
      status: 'working',
      turns: [
        ...idleSession().turns,
        {
          id: 'provider-turn',
          turn_count: 2,
          status: 'running',
          provider_turn_started: true,
          provider_resume_at: null,
          started_at: 150,
          completed_at: null,
          checkpoint: null,
        },
      ],
    }
    const result = reduceRuntimeEvent(session, event('promptSubmitted', SUBMISSION), clock)

    expect(result.session.turns).toHaveLength(2)
    expect(result.session.messages.at(-1)).toMatchObject({
      id: SUBMISSION.messageId,
      turn_id: 'provider-turn',
      role: 'user',
      content: 'Second prompt',
    })
  })

  test('names an unnamed task after its first submitted prompt', () => {
    const session: AgentSession = { ...idleSession(), messages: [], turns: [] }
    const result = reduceRuntimeEvent(
      session,
      event('promptSubmitted', { ...SUBMISSION, message: 'Add a dark mode toggle to settings' }),
      clock,
    )

    expect(result.session.auto_title).toBe('Add a dark mode toggle to settings')
    expect(result.session.turns.at(-1)?.turn_count).toBe(1)
  })
})

test('MCP tool identity survives partial updates and stays separate in expanded details', () => {
  const call = {
    id: 'tool-1', source_id: 'call-1', kind: 'tool' as const,
    title: 'List running apps via CUA', detail: null,
    tool_name: 'js', mcp_server: 'waku_js_repl', arguments: '{}',
    failed: false, complete: false,
  }
  const started = apply(runningSession(), 'richActivity', call)
  const completed = apply(started, 'richActivity', {
    ...call, id: 'update-1', tool_name: null, mcp_server: null, arguments: null,
    output: '2 apps', complete: true,
  })
  const content = completed.transcript_blocks[0]!.content
  if (content.kind !== 'activities') throw new Error('Expected an activity block')
  const item = content.data[0]!
  expect(item.id).toBe('tool-1')
  expect(item.title).toBe('List running apps via CUA')
  expect(activityDisclosureSections(item)).toEqual([
    { kind: 'mcp-server', label: 'MCP server', content: 'waku_js_repl' },
    { kind: 'tool-name', label: 'Tool', content: 'js' },
    { kind: 'arguments', label: 'Arguments', content: '{}' },
    { kind: 'output', label: 'Output', content: '2 apps' },
  ])
  expect(activityDisclosureSections({
    ...call, mcp_server: null, tool_name: 'read_file', arguments: null, detail: 'Permission denied',
  })).toEqual([
    { kind: 'tool-name', label: 'Tool', content: 'read_file' },
    { kind: 'detail', label: null, content: 'Permission denied' },
  ])
})

function apply(session: AgentSession, kind: string, payload: unknown) {
  return reduceRuntimeEvent(session, event(kind, payload), clock).session
}

function event(kind: string, payload: unknown): SequencedEvent {
  return {
    sessionId: 'session',
    runtimeId: 'runtime',
    epoch: 'epoch',
    sequence: 1,
    event: { kind, payload: payload as never },
  }
}

/** One completed turn, runtime still attached, nothing running. */
function idleSession(): AgentSession {
  return {
    ...runningSession(),
    status: 'idle',
    messages: [
      {
        id: 'message',
        turn_id: 'turn',
        role: 'user',
        content: 'Go',
        created_at: 100,
        streaming: false,
      },
      {
        id: 'reply',
        turn_id: 'turn',
        role: 'assistant',
        content: 'Done',
        created_at: 110,
        streaming: false,
      },
    ],
    turns: [
      {
        id: 'turn',
        turn_count: 1,
        status: 'completed',
        provider_turn_started: true,
        provider_resume_at: null,
        started_at: 100,
        completed_at: 110,
        checkpoint: null,
      },
    ],
  }
}

function runningSession(): AgentSession {
  return {
    id: 'session',
    title: 'New task',
    project_id: 'project',
    workspace: { kind: 'local' },
    provider: 'codex',
    runtime_mode: 'fullAccess',
    status: 'connecting',
    created_at: 100,
    updated_at: 100,
    provider_cursor: null,
    messages: [
      {
        id: 'message',
        turn_id: 'turn',
        role: 'user',
        content: 'Go',
        created_at: 100,
        streaming: false,
      },
    ],
    transcript_blocks: [],
    turns: [
      {
        id: 'turn',
        turn_count: 1,
        status: 'running',
        provider_turn_started: false,
        provider_resume_at: null,
        started_at: 100,
        completed_at: null,
        checkpoint: null,
      },
    ],
  }
}

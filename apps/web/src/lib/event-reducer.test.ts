import { describe, expect, test } from 'bun:test'
import type { AgentSession, SequencedEvent } from '@waku/client'
import { reduceRuntimeEvent } from './event-reducer'

const clock = {
  nowSeconds: () => 200,
  nowMillis: () => 200_000,
  randomUUID: (() => {
    let id = 0
    return () => `00000000-0000-4000-8000-${String(++id).padStart(12, '0')}`
  })(),
}

describe('reduceRuntimeEvent', () => {
  test('preserves event order across reasoning, tools, and assistant text', () => {
    let session = runningSession()
    session = apply(session, 'reasoningDelta', 'Thinking')
    session = apply(session, 'richActivity', {
      id: '10000000-0000-4000-8000-000000000001',
      source_id: 'tool-1',
      kind: 'fileRead',
      title: 'Read file',
      detail: 'src/app.rs',
      failed: false,
      complete: true,
    })
    session = apply(session, 'textDelta', 'Done')
    session = apply(session, 'textDelta', '.')

    expect(session.transcript_blocks[0]?.content).toMatchObject({
      kind: 'activities',
      data: [
        { reasoning: { content: 'Thinking' }, complete: true },
        { source_id: 'tool-1', title: 'Read file' },
      ],
    })
    expect(session.messages.at(-1)?.content).toBe('Done.')
  })

  test('settles the active turn and finalizes streaming output', () => {
    let session = runningSession()
    session = apply(session, 'textDelta', 'Ready')
    const result = reduceRuntimeEvent(
      session,
      event('turnFinished', { success: true, summary: null }),
      clock,
    )

    expect(result.settled).toBe(true)
    expect(result.session.status).toBe('idle')
    expect(result.session.turns[0]?.status).toBe('completed')
    expect(result.session.messages.at(-1)?.streaming).toBe(false)
  })

  test('does not turn a completed session into a failure when its process exits', () => {
    let session = runningSession()
    session = reduceRuntimeEvent(
      session,
      event('turnFinished', { success: true, summary: null }),
      clock,
    ).session
    const result = reduceRuntimeEvent(session, event('processExited', null), clock)

    expect(result.session.status).toBe('idle')
    expect(result.session.turns[0]?.status).toBe('completed')
    expect(result.settled).toBe(false)
    expect(result.removeRuntime).toBe(true)
  })

  test('stores the daemon sequence incorporated into the transcript', () => {
    const wire = { ...event('textDelta', 'hello'), sequence: 42 }
    const result = reduceRuntimeEvent(runningSession(), wire, clock)

    expect(result.session.runtime_event_cursor).toEqual({
      runtime_id: 'runtime',
      epoch: 'epoch',
      sequence: 42,
    })
  })

  test('records Claude resume position on the active turn', () => {
    const result = reduceRuntimeEvent(
      runningSession(),
      event('connected', {
        provider: 'claude',
        sessionId: 'provider-session',
        resumeAt: 'provider-message',
      }),
      clock,
    )

    expect(result.session.turns[0]?.provider_resume_at).toBe('provider-message')
  })

  test('ignores late turn output and permission events after a turn settles', () => {
    let session = reduceRuntimeEvent(
      runningSession(),
      event('turnFinished', { success: true, summary: null }),
      clock,
    ).session
    const messageCount = session.messages.length
    session = apply(session, 'textDelta', 'late output')
    session = apply(session, 'richActivity', {
      id: 'late-tool',
      source_id: 'late-tool',
      kind: 'tool',
      title: 'Late tool',
      failed: false,
      complete: true,
    })
    const permission = reduceRuntimeEvent(
      session,
      event('permission', { requestId: 'late', title: 'Late', detail: '', options: [] }),
      clock,
    )

    expect(permission.session.messages).toHaveLength(messageCount)
    expect(permission.session.transcript_blocks).toHaveLength(0)
    expect(permission.session.status).toBe('idle')
    expect(permission.permission).toBeUndefined()
  })

  test('keeps the provider error when a working runtime exits', () => {
    let session = apply(runningSession(), 'turnStarted', null)
    const errored = reduceRuntimeEvent(session, event('error', 'provider exploded'), clock)
    expect(errored.session.status).toBe('working')
    session = reduceRuntimeEvent(
      errored.session,
      event('processExited', null),
      clock,
      errored.error,
    ).session

    expect(session.status).toBe('failed')
    expect(session.messages.at(-1)?.content).toBe('provider exploded')
  })

  test('surfaces a startup error immediately without duplicating it on exit', () => {
    const errored = reduceRuntimeEvent(
      runningSession(),
      event('error', 'could not start provider'),
      clock,
    )
    expect(errored.session.status).toBe('failed')
    expect(errored.session.messages.at(-1)?.content).toBe('could not start provider')

    const exited = reduceRuntimeEvent(
      errored.session,
      event('processExited', null),
      clock,
      errored.error,
    )
    expect(exited.session.messages.filter((message) => message.role === 'assistant')).toHaveLength(1)
  })
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

function runningSession(): AgentSession {
  return {
    id: 'session',
    title: 'New task',
    project_id: 'project',
    workspace: { kind: 'local' },
    provider: 'codex',
    runtime_mode: 'fullAccess',
    interaction_mode: 'build',
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
        started_at: 100,
        completed_at: null,
        checkpoint: null,
      },
    ],
  }
}

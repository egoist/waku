import type {
  ActivityItem,
  ActivityKind,
  AgentSession,
  ProviderResumeCursor,
  ReportedCommand,
  SequencedEvent,
  TranscriptBlock,
  TurnStatus,
} from '@waku/client'

export interface PendingPermission {
  requestId: string
  title: string
  detail: string
  options: Array<{ id: string; label: string; allow: boolean }>
}

export interface RuntimeEventResult {
  session: AgentSession
  permission?: PendingPermission | null
  settled: boolean
  removeRuntime: boolean
  error?: string
}

interface ReducerClock {
  nowSeconds: () => number
  nowMillis: () => number
  randomUUID: () => string
}

const defaultClock: ReducerClock = {
  nowSeconds: () => Math.floor(Date.now() / 1_000),
  nowMillis: () => Date.now(),
  randomUUID: () => crypto.randomUUID(),
}

export function reduceRuntimeEvent(
  current: AgentSession,
  wire: SequencedEvent,
  clock: ReducerClock = defaultClock,
  processExitError: string | null = null,
): RuntimeEventResult {
  const session = clone(current)
  const { kind, payload } = wire.event
  const result: RuntimeEventResult = {
    session,
    settled: false,
    removeRuntime: false,
  }

  session.runtime_event_cursor = {
    runtime_id: wire.runtimeId,
    epoch: wire.epoch,
    sequence: wire.sequence,
  }

  switch (kind) {
    case 'connected':
      session.provider_cursor = (payload as ProviderResumeCursor | null) ?? null
      if (
        session.provider_cursor?.provider === 'claude'
          && session.provider_cursor.resumeAt
      ) {
        const turn = activeTurn(session)
        if (turn) turn.provider_resume_at = session.provider_cursor.resumeAt
      }
      if (session.status === 'connecting') session.status = 'working'
      break
    case 'agentPresetSelected':
      session.agent_preset = typeof payload === 'string' ? payload : null
      break
    case 'autoTitleUpdated':
      session.auto_title = typeof payload === 'string' && payload.trim() ? payload.trim() : null
      break
    case 'availableCommands':
      if (Array.isArray(payload)) session.available_commands = payload as ReportedCommand[]
      break
    case 'turnStarted': {
      const turn = activeTurn(session)
      if (turn) {
        turn.provider_turn_started = true
        session.status = 'working'
      }
      break
    }
    case 'textDelta':
      if (typeof payload === 'string' && acceptsTurnOutput(session)) {
        appendText(session, payload, clock)
      }
      break
    case 'reasoningDelta':
      if (typeof payload === 'string' && acceptsTurnOutput(session)) {
        appendReasoning(session, payload, clock)
      }
      break
    case 'activity': {
      const value = asRecord(payload)
      if (!acceptsTurnOutput(session) || !value || typeof value.title !== 'string') break
      upsertActivity(
        session,
        {
          id: clock.randomUUID(),
          source_id: typeof value.id === 'string' ? value.id : null,
          kind: isActivityKind(value.kind) ? value.kind : 'tool',
          title: value.title,
          detail: typeof value.detail === 'string' ? value.detail : null,
          arguments: null,
          output: null,
          image_urls: [],
          failed: false,
          complete: value.complete === true,
          file_changes: [],
          display_target: null,
          reasoning: null,
        },
        clock,
      )
      break
    }
    case 'richActivity':
      if (acceptsTurnOutput(session) && asRecord(payload)) {
        upsertActivity(session, payload as ActivityItem, clock)
      }
      break
    case 'permission': {
      const value = asRecord(payload)
      if (!acceptsTurnOutput(session) || !value || typeof value.requestId !== 'string') break
      result.permission = {
        requestId: value.requestId,
        title: typeof value.title === 'string' ? value.title : 'Permission required',
        detail: typeof value.detail === 'string' ? value.detail : '',
        options: Array.isArray(value.options)
          ? value.options.filter(isPermissionOption)
          : [],
      }
      session.status = 'waiting'
      break
    }
    case 'usageUpdated': {
      const value = asRecord(payload)
      if (!value) break
      const previous = session.context_usage ?? { tokens: 0, window: null }
      session.context_usage = {
        tokens:
          typeof value.contextTokens === 'number' ? value.contextTokens : previous.tokens,
        window:
          typeof value.contextWindow === 'number'
            ? value.contextWindow
            : previous.window,
      }
      break
    }
    case 'turnFinished': {
      const value = asRecord(payload)
      const success = value?.success === true
      result.settled = settleTurn(
        session,
        success ? 'completed' : 'failed',
        typeof value?.summary === 'string' ? value.summary : null,
        clock,
      )
      result.permission = null
      break
    }
    case 'error': {
      if (typeof payload !== 'string') break
      result.error = payload
      const turn = activeTurn(session)
      if (!turn || session.status === 'working') break
      const hasAssistant = session.messages.some(
        (message) => message.turn_id === turn.id && message.role === 'assistant',
      )
      session.status = 'failed'
      if (!hasAssistant) {
        session.messages.push({
          id: clock.randomUUID(),
          turn_id: turn.id,
          role: 'assistant',
          content: payload,
          created_at: clock.nowSeconds(),
          streaming: false,
        })
      }
      break
    }
    case 'processExited':
      result.settled = settleTurn(
        session,
        'failed',
        processExitError ?? 'The agent exited before responding.',
        clock,
      )
      result.permission = null
      result.removeRuntime = true
      break
    default:
      break
  }

  session.updated_at = clock.nowSeconds()
  return result
}

function appendText(session: AgentSession, delta: string, clock: ReducerClock) {
  if (!delta) return
  completeReasoning(session)
  const previous = session.messages.at(-1)
  if (previous?.role === 'assistant' && previous.streaming) {
    previous.content += delta
  } else {
    session.messages.push({
      id: clock.randomUUID(),
      turn_id: activeTurn(session)?.id ?? null,
      role: 'assistant',
      content: delta,
      created_at: clock.nowSeconds(),
      streaming: true,
    })
  }
}

function appendReasoning(session: AgentSession, delta: string, clock: ReducerClock) {
  if (!delta.trim() && !lastReasoning(session)) return
  finishStreamingMessages(session)
  const existing = lastReasoning(session)
  if (existing && !existing.activity.complete) {
    existing.activity.reasoning!.content += delta
    existing.activity.reasoning!.finished_at_ms = clock.nowMillis()
    return
  }
  const now = clock.nowMillis()
  pushActivity(session, {
    id: clock.randomUUID(),
    source_id: null,
    kind: 'reasoning',
    title: 'Reasoning',
    detail: null,
    arguments: null,
    output: null,
    image_urls: [],
    failed: false,
    complete: false,
    file_changes: [],
    display_target: null,
    reasoning: { content: delta, started_at_ms: now, finished_at_ms: now },
  })
}

function upsertActivity(
  session: AgentSession,
  incoming: ActivityItem,
  _clock: ReducerClock,
) {
  finishStreamingMessages(session)
  completeReasoning(session)
  for (const block of [...session.transcript_blocks].reverse()) {
    const activities = ensureActivities(block)
    const matching = [...activities].reverse().find((activity) =>
      incoming.source_id
        ? activity.source_id === incoming.source_id
        : activity.title === incoming.title && !activity.complete,
    )
    if (!matching) continue
    Object.assign(matching, {
      ...incoming,
      id: matching.id,
      detail: incoming.detail ?? matching.detail,
      arguments: incoming.arguments ?? matching.arguments,
      output: incoming.output ?? matching.output,
      image_urls: incoming.image_urls?.length ? incoming.image_urls : matching.image_urls,
      file_changes: incoming.file_changes?.length
        ? incoming.file_changes
        : matching.file_changes,
      display_target: incoming.display_target ?? matching.display_target,
      reasoning: incoming.reasoning ?? matching.reasoning,
    })
    return
  }
  pushActivity(session, incoming)
}

function pushActivity(session: AgentSession, activity: ActivityItem) {
  const afterMessage = session.messages.length
  const turnId = activeTurn(session)?.id ?? null
  const last = session.transcript_blocks.at(-1)
  if (last && last.after_message === afterMessage && last.turn_id === turnId) {
    ensureActivities(last).push(activity)
    return
  }
  session.transcript_blocks.push({
    after_message: afterMessage,
    turn_id: turnId,
    content: { kind: 'activities', data: [activity] },
  })
}

function settleTurn(
  session: AgentSession,
  status: TurnStatus,
  fallback: string | null,
  clock: ReducerClock,
): boolean {
  finishStreamingMessages(session)
  completeActivities(session)
  const turn = activeTurn(session)
  if (!turn) return false
  const hasAssistant = session.messages.some(
    (message) => message.turn_id === turn.id && message.role === 'assistant',
  )
  if (!hasAssistant) {
    session.messages.push({
      id: clock.randomUUID(),
      turn_id: turn.id,
      role: 'assistant',
      content:
        fallback ??
        (status === 'completed'
          ? 'The turn completed without a text response.'
          : 'The turn stopped before a response.'),
      created_at: clock.nowSeconds(),
      streaming: false,
    })
  }
  turn.status = status
  turn.completed_at = clock.nowSeconds()
  session.last_reply_at = turn.completed_at
  session.status = status === 'completed' ? 'idle' : 'failed'
  return true
}

function finishStreamingMessages(session: AgentSession) {
  for (const message of session.messages) {
    if (message.role === 'assistant') message.streaming = false
  }
}

function completeReasoning(session: AgentSession) {
  const reasoning = lastReasoning(session)
  if (reasoning) reasoning.activity.complete = true
}

function completeActivities(session: AgentSession) {
  for (const block of session.transcript_blocks) {
    for (const activity of ensureActivities(block)) activity.complete = true
  }
}

function lastReasoning(session: AgentSession) {
  const block = session.transcript_blocks.at(-1)
  const activity = block ? ensureActivities(block).at(-1) : undefined
  return activity?.reasoning ? { activity } : null
}

export function activitiesForBlock(block: TranscriptBlock): ActivityItem[] {
  if (block.content.kind === 'activities') return block.content.data
  const reasoning = block.content.data
  return [
    {
      id: `legacy-reasoning-${block.after_message}`,
      source_id: null,
      kind: 'reasoning',
      title: 'Reasoning',
      detail: null,
      arguments: null,
      output: null,
      image_urls: [],
      failed: false,
      complete: true,
      file_changes: [],
      display_target: null,
      reasoning,
    },
  ]
}

function ensureActivities(block: TranscriptBlock): ActivityItem[] {
  if (block.content.kind === 'activities') return block.content.data
  const activities = activitiesForBlock(block)
  block.content = { kind: 'activities', data: activities }
  return activities
}

function activeTurn(session: AgentSession) {
  const turn = session.turns.at(-1)
  return turn?.status === 'running' ? turn : undefined
}

function acceptsTurnOutput(session: AgentSession) {
  return Boolean(
    activeTurn(session)
      && ['connecting', 'working', 'waiting'].includes(session.status),
  )
}

function isActivityKind(value: unknown): value is ActivityKind {
  return (
    typeof value === 'string' &&
    [
      'reasoning',
      'command',
      'fileChange',
      'fileRead',
      'fileSearch',
      'fileList',
      'search',
      'plan',
      'tool',
    ].includes(value)
  )
}

function isPermissionOption(
  value: unknown,
): value is { id: string; label: string; allow: boolean } {
  const option = asRecord(value)
  return Boolean(
    option &&
      typeof option.id === 'string' &&
      typeof option.label === 'string' &&
      typeof option.allow === 'boolean',
  )
}

function asRecord(value: unknown): Record<string, unknown> | null {
  return value !== null && typeof value === 'object' && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : null
}

function clone<T>(value: T): T {
  return JSON.parse(JSON.stringify(value)) as T
}

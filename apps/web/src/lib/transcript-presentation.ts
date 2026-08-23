import type { ActivityItem, AgentSession } from '@waku/client'

export type AssistantResponseFooter = {
  content: string
  timestamp: number
}

export type Translator = (key: string, params?: Record<string, string | number>) => string

export function userMessageRewindTurnCount(
  session: AgentSession,
  message: AgentSession['messages'][number],
  availableRetainedTurnCounts: ReadonlySet<number>,
): number | null {
  if (message.role !== 'user' || !['idle', 'failed'].includes(session.status)) return null
  const turn = message.turn_id
    ? session.turns.find((candidate) => candidate.id === message.turn_id)
    : undefined
  if (!turn) return null
  const retainedTurnCount = Math.max(0, turn.turn_count - 1)
  if (!availableRetainedTurnCounts.has(retainedTurnCount)) return null
  const rollbackTurns = session.turns
    .slice(retainedTurnCount)
    .filter((candidate) => candidate.provider_turn_started)
    .length
  if (rollbackTurns > 0 && !session.provider_cursor) return null
  return turn.turn_count
}

export function activitySummary(activities: ActivityItem[], t?: Translator) {
  const counts = new Map<ActivityItem['kind'], number>()
  for (const activity of activities) {
    counts.set(activity.kind, (counts.get(activity.kind) ?? 0) + 1)
  }
  const parts = [...counts].map(([kind, count]) => t
    ? t('activity.count', { count, activity: t(activityNounKey(kind, count)) })
    : `${count} ${activityNoun(kind, count)}`)
  const joined = parts.join(' · ')
  return t
    ? t(activities.some((activity) => !activity.complete) ? 'activity.running' : 'activity.ran', { activities: joined })
    : `${activities.some((activity) => !activity.complete) ? 'Running' : 'Ran'} ${joined}`
}

export function activityGroupIsLive(
  liveTurn: boolean,
  latestBlock: boolean,
  afterMessage: number,
  messageCount: number,
) {
  return liveTurn && latestBlock && afterMessage === messageCount
}

export function activityHeaderTitle(activities: ActivityItem[], liveGroup: boolean, t?: Translator) {
  const latest = liveGroup ? activities.at(-1) : undefined
  if (latest) return latest.reasoning ? reasoningTitle(latest, t) : activityDisplayTitle(latest, t)
  return activitySummary(activities, t)
}

export function reasoningTitle(activity: ActivityItem, t?: Translator) {
  const reasoning = activity.reasoning
  if (!reasoning) return activity.title
  if (!activity.complete) return t ? t('transcript.thinking') : 'Thinking'
  const seconds = Math.max(1, Math.ceil((reasoning.finished_at_ms - reasoning.started_at_ms) / 1_000))
  return t
    ? t('transcript.thought_for', { duration: formatDuration(seconds, t) })
    : `Thought for ${formatDuration(seconds)}`
}

export function activityDisplayTitle(activity: ActivityItem, t?: Translator) {
  const target = activity.display_target?.trim() || null
  switch (activity.kind) {
    case 'fileChange': {
      const changes = activity.file_changes ?? []
      const subject = changes.length === 1
        ? pathName(changes[0]!.path)
        : changes.length > 1
          ? t ? t('activity.file_count', { count: changes.length }) : `${changes.length} files`
          : null
      if (!subject && !isGenericActivityTitle(activity)) return activity.title
      if (!activity.complete) return t
        ? t(subject ? 'activity.editing_named_file' : 'activity.editing_files', subject ? { file: subject } : undefined)
        : subject ? `Editing ${subject}` : 'Editing files'
      if (activity.failed) return t
        ? t(subject ? 'activity.edit_failed_named_file' : 'activity.edit_failed', subject ? { file: subject } : undefined)
        : subject ? `Failed to edit ${subject}` : 'Failed to edit files'
      return t
        ? t(subject ? 'activity.edited_named_file' : 'activity.edited_files', subject ? { file: subject } : undefined)
        : subject ? `Edited ${subject}` : 'Edited files'
    }
    case 'fileRead': {
      const file = target ? pathName(target) : null
      if (!file && !isGenericActivityTitle(activity)) return activity.title
      if (!activity.complete) return t
        ? t(file ? 'activity.reading_named_file' : 'activity.reading_file', file ? { file } : undefined)
        : file ? `Reading ${file}` : 'Reading file'
      if (activity.failed) return t
        ? t(file ? 'activity.read_named_file_failed' : 'activity.read_file_failed', file ? { file } : undefined)
        : file ? `Failed to read ${file}` : 'Failed to read file'
      return t
        ? t(file ? 'activity.read_named_file' : 'activity.read_file_completed', file ? { file } : undefined)
        : file ? `Read ${file}` : 'Read file'
    }
    case 'fileSearch': {
      if (!target && !isGenericActivityTitle(activity)) return activity.title
      if (!activity.complete) return t
        ? t(target ? 'activity.searching_files_for' : 'activity.searching_files', target ? { query: target } : undefined)
        : target ? `Searching files for ${target}` : 'Searching files'
      if (activity.failed) return t
        ? t(target ? 'activity.file_search_failed_for' : 'activity.file_search_failed', target ? { query: target } : undefined)
        : target ? `Failed to search files for ${target}` : 'Failed to search files'
      return t
        ? t(target ? 'activity.searched_files_for' : 'activity.searched_files', target ? { query: target } : undefined)
        : target ? `Searched files for ${target}` : 'Searched files'
    }
    case 'fileList': {
      const directory = target ? pathName(target) : null
      if (!directory && !isGenericActivityTitle(activity)) return activity.title
      if (!activity.complete) return t
        ? t(directory ? 'activity.listing_files_in' : 'activity.listing_files', directory ? { directory } : undefined)
        : directory ? `Listing files in ${directory}` : 'Listing files'
      if (activity.failed) return t
        ? t(directory ? 'activity.file_list_failed_in' : 'activity.file_list_failed', directory ? { directory } : undefined)
        : directory ? `Failed to list files in ${directory}` : 'Failed to list files'
      return t
        ? t(directory ? 'activity.listed_files_in' : 'activity.listed_files', directory ? { directory } : undefined)
        : directory ? `Listed files in ${directory}` : 'Listed files'
    }
    case 'command':
      if (activity.display_description?.trim()) {
        if (!activity.complete) return t
          ? t('activity.running_described_command', { description: activity.display_description })
          : `Running command: ${activity.display_description}`
        return activity.failed
          ? t
            ? t('activity.described_command_failed', { description: activity.display_description })
            : `Command failed: ${activity.display_description}`
          : t
            ? t('activity.ran_described_command', { description: activity.display_description })
            : `Ran command: ${activity.display_description}`
      }
      if (target) {
        if (!activity.complete) return t ? t('activity.running_named_command', { command: target }) : `Running ${target}`
        return activity.failed
          ? t ? t('activity.named_command_failed', { command: target }) : `Command failed: ${target}`
          : t ? t('activity.ran_named_command', { command: target }) : `Ran ${target}`
      }
      if (!isGenericActivityTitle(activity)) return activity.title
      if (!activity.complete) return t ? t('activity.running_command') : 'Running command'
      return activity.failed
        ? t ? t('activity.command_failed') : 'Command failed'
        : t ? t('activity.ran_command') : 'Ran command'
    case 'search':
      if (target) {
        if (!activity.complete) return t ? t('activity.searching_web_for', { query: target }) : `Searching the web for ${target}`
        return activity.failed
          ? t ? t('activity.web_search_failed_for', { query: target }) : `Failed to search the web for ${target}`
          : t ? t('activity.searched_web_for', { query: target }) : `Searched the web for ${target}`
      }
      if (!isGenericActivityTitle(activity)) return activity.title
      if (!activity.complete) return t ? t('activity.searching_web') : 'Searching the web'
      return activity.failed
        ? t ? t('activity.web_search_failed') : 'Failed to search the web'
        : t ? t('activity.searched_the_web') : 'Searched the web'
    case 'plan':
      if (!isGenericActivityTitle(activity)) return activity.title
      if (!activity.complete) return t ? t('activity.updating_plan') : 'Updating plan'
      return activity.failed
        ? t ? t('activity.plan_update_failed') : 'Failed to update plan'
        : t ? t('activity.updated_plan') : 'Updated plan'
    case 'tool':
      return activityToolDisplayName(activity, t)
    case 'reasoning':
      return activity.title
  }
}

export function activityActionLabel(activity: ActivityItem, t?: Translator) {
  if (isAskUserQuestion(activity)) {
    return t ? t('activity.ask_questions') : 'Ask questions'
  }
  const key = activity.kind === 'reasoning'
    ? 'activity.action_think'
    : activity.kind === 'command'
      ? 'activity.action_run'
      : activity.kind === 'fileChange'
        ? 'activity.action_edit'
        : activity.kind === 'fileRead'
          ? 'activity.action_read'
          : activity.kind === 'fileSearch' || activity.kind === 'search'
            ? 'activity.action_search'
            : activity.kind === 'fileList'
              ? 'activity.action_list'
              : activity.kind === 'plan'
                ? 'activity.action_plan'
                : 'activity.tool'
  if (t) return t(key)
  return {
    'activity.action_think': 'Think',
    'activity.action_run': 'Run',
    'activity.action_edit': 'Edit',
    'activity.action_read': 'Read',
    'activity.action_search': 'Search',
    'activity.action_list': 'List',
    'activity.action_plan': 'Plan',
    'activity.tool': 'Tool',
  }[key]!
}

export function activityRowDetail(activity: ActivityItem, t?: Translator) {
  const customTitle = !isGenericActivityTitle(activity) ? activity.title : ''
  switch (activity.kind) {
    case 'reasoning':
      return reasoningTitle(activity, t)
    case 'command':
      return activity.display_description?.trim() || activity.display_target?.trim() || customTitle
    case 'fileChange': {
      const changes = activity.file_changes ?? []
      if (changes.length === 1) return pathName(changes[0]!.path)
      if (changes.length > 1) return t
        ? t('activity.file_count', { count: changes.length })
        : `${changes.length} files`
      return customTitle
    }
    case 'fileRead':
    case 'fileList':
      return activity.display_target?.trim()
        ? pathName(activity.display_target)
        : customTitle
    case 'fileSearch':
      return activityDisplayTitle(activity, t)
    case 'search':
      return activity.display_target?.trim()
        ? t
          ? t('activity.search_for', { query: activity.display_target })
          : `Search for ${activity.display_target}`
        : customTitle
    case 'plan':
      return customTitle
    case 'tool':
      if (isAskUserQuestion(activity)) return ''
      return activity.display_target?.trim() || !isGenericActivityTitle(activity)
        ? activityToolDisplayName(activity, t)
        : ''
  }
}

export type ActivityDisclosureSection = {
  kind: 'command' | 'arguments' | 'output' | 'detail'
  label: string | null
  content: string
}

export function activityDisclosureSections(activity: ActivityItem, t?: Translator): ActivityDisclosureSection[] {
  const sections: ActivityDisclosureSection[] = []
  if (activity.kind === 'command') {
    const command = activity.arguments?.trim() || activity.display_target?.trim()
    const output = activity.output?.trim()
    if (command) sections.push({ kind: 'command', label: t ? t('activity.command_detail') : 'Command', content: command })
    if (output) sections.push({ kind: 'output', label: t ? t('activity.output') : 'Output', content: output })
    else if (activity.image_urls?.length) sections.push({ kind: 'output', label: t ? t('activity.output') : 'Output', content: '' })
    return sections
  }
  const argumentsText = activity.arguments?.trim()
  const output = activity.output?.trim()
  if (argumentsText) sections.push({ kind: 'arguments', label: t ? t('activity.arguments') : 'Arguments', content: argumentsText })
  if (output) sections.push({ kind: 'output', label: t ? t('activity.output') : 'Output', content: output })
  else if (activity.image_urls?.length) sections.push({ kind: 'output', label: t ? t('activity.output') : 'Output', content: '' })
  const detail = activity.detail?.trim()
  if (!sections.length && detail) sections.push({ kind: 'detail', label: null, content: detail })
  return sections
}

export function activityPreview(activity: ActivityItem, t?: Translator) {
  const detail = activity.detail?.trim() ?? ''
  if (detail.toLocaleLowerCase() === 'failed') {
    const firstOutputLine = activity.output?.split('\n').find((line) => line.trim())?.trim()
    if (firstOutputLine) return firstOutputLine
  }
  if ((!detail || detail.toLocaleLowerCase() === 'failed') && activity.image_urls?.length) {
    return t ? t('activity.image_output') : 'Image output'
  }
  return detail
}

export function activityFileChangeStats(activity: ActivityItem) {
  if (activity.kind !== 'fileChange' || !activity.complete || activity.failed) return null
  const changes = activity.file_changes ?? []
  if (!changes.length || changes.some((change) => change.additions == null || change.deletions == null)) {
    return null
  }
  return {
    additions: changes.reduce((total, change) => total + change.additions!, 0),
    deletions: changes.reduce((total, change) => total + change.deletions!, 0),
  }
}

export function turnFoldLabel(turn: AgentSession['turns'][number], t?: Translator) {
  const seconds = Math.max(1, (turn.completed_at ?? Math.floor(Date.now() / 1_000)) - turn.started_at)
  if (t) {
    return t(turn.status === 'interrupted' ? 'transcript.you_stopped_after' : 'transcript.worked_for', {
      duration: formatDuration(seconds, t),
    })
  }
  return turn.status === 'interrupted'
    ? `You stopped after ${formatDuration(seconds)}`
    : `Worked for ${formatDuration(seconds)}`
}

export function formatDuration(seconds: number, t?: Translator) {
  if (seconds < 60) return t
    ? t(seconds === 1 ? 'duration.second' : 'duration.seconds', { count: seconds })
    : `${seconds} ${seconds === 1 ? 'second' : 'seconds'}`
  if (seconds < 3_600) {
    const minutes = Math.floor(seconds / 60)
    const remaining = seconds % 60
    const first = t
      ? t(minutes === 1 ? 'duration.minute' : 'duration.minutes', { count: minutes })
      : `${minutes} ${minutes === 1 ? 'minute' : 'minutes'}`
    return remaining
      ? t
        ? t('duration.two_units', {
            first,
            second: t(remaining === 1 ? 'duration.second' : 'duration.seconds', { count: remaining }),
          })
        : `${first} ${remaining} ${remaining === 1 ? 'second' : 'seconds'}`
      : first
  }
  const hours = Math.floor(seconds / 3_600)
  const minutes = Math.floor((seconds % 3_600) / 60)
  const first = t
    ? t(hours === 1 ? 'duration.hour' : 'duration.hours', { count: hours })
    : `${hours} ${hours === 1 ? 'hour' : 'hours'}`
  return minutes
    ? t
      ? t('duration.two_units', {
          first,
          second: t(minutes === 1 ? 'duration.minute' : 'duration.minutes', { count: minutes }),
        })
      : `${first} ${minutes} ${minutes === 1 ? 'minute' : 'minutes'}`
    : first
}

export function formatWorkingElapsed(seconds: number, t?: Translator) {
  if (seconds < 60) return t ? t('duration.seconds_short', { count: seconds }) : `${seconds}s`
  if (seconds < 3_600) {
    const minutes = Math.floor(seconds / 60)
    const remaining = seconds % 60
    const first = t ? t('duration.minutes_short', { count: minutes }) : `${minutes}m`
    return remaining
      ? t
        ? t('duration.two_units', { first, second: t('duration.seconds_short', { count: remaining }) })
        : `${minutes}m ${remaining}s`
      : first
  }
  const hours = Math.floor(seconds / 3_600)
  const minutes = Math.floor((seconds % 3_600) / 60)
  const first = t ? t('duration.hours_short', { count: hours }) : `${hours}h`
  return minutes
    ? t
      ? t('duration.two_units', { first, second: t('duration.minutes_short', { count: minutes }) })
      : `${hours}h ${minutes}m`
    : first
}

export function formatMessageTime(
  timestamp: number,
  now = new Date(),
  locale?: Intl.LocalesArgument,
) {
  const date = new Date(timestamp * 1_000)
  if (Number.isNaN(date.getTime())) return ''

  const time = new Intl.DateTimeFormat(locale, {
    hour: 'numeric',
    minute: '2-digit',
  }).format(date)
  const dateOnly = localDateNumber(date)
  const today = localDateNumber(now)
  if (dateOnly >= today) return time

  const yesterday = new Date(now.getFullYear(), now.getMonth(), now.getDate() - 1)
  if (sameLocalDate(date, yesterday)) {
    const label = new Intl.RelativeTimeFormat(locale, { numeric: 'auto' }).format(-1, 'day')
    return `${capitalize(label)} ${time}`
  }

  const weekStart = new Date(now.getFullYear(), now.getMonth(), now.getDate())
  weekStart.setDate(weekStart.getDate() - ((weekStart.getDay() + 6) % 7))
  if (dateOnly >= localDateNumber(weekStart)) {
    const weekday = new Intl.DateTimeFormat(locale, { weekday: 'long' }).format(date)
    return `${weekday} ${time}`
  }

  if (language(locale) === 'en') {
    const month = new Intl.DateTimeFormat(locale, { month: 'short' }).format(date)
    const day = date.getDate()
    const year = date.getFullYear() === now.getFullYear() ? '' : ` ${date.getFullYear()}`
    return `${month} ${day}${ordinalSuffix(day)}${year}, ${time}`
  }
  const calendar = new Intl.DateTimeFormat(locale, {
    month: 'short',
    day: 'numeric',
    ...(date.getFullYear() === now.getFullYear() ? {} : { year: 'numeric' }),
  }).format(date)
  return `${calendar} ${time}`
}

export function fencedCode(content: string): string | null {
  const codeBlocks: string[] = []
  const segments = content.split('```')
  for (let index = 1; index < segments.length; index += 2) {
    const fenced = segments[index] ?? ''
    const newline = fenced.indexOf('\n')
    const code = newline === -1 ? fenced : fenced.slice(newline + 1)
    const normalized = code.trimEnd()
    if (normalized.trim()) codeBlocks.push(normalized)
  }
  return codeBlocks.length ? codeBlocks.join('\n\n') : null
}

export function activityTextRows(content: string): string[] {
  const rows: string[] = []
  for (const line of content.split(/\r?\n/)) {
    if (line.length <= 2_000) {
      rows.push(line)
      continue
    }
    for (let start = 0; start < line.length; start += 2_000) {
      rows.push(line.slice(start, start + 2_000))
    }
  }
  return rows
}

export function shouldVirtualizeActivityText(content: string, rows: readonly string[]) {
  return rows.length > 200 || content.length > 40_000
}

/** Matches Desktop's boundary between folded work and the visible answer. */
export function turnAnswerStart<T>(
  rows: readonly T[],
  isAnswerText: (row: T, index: number) => boolean,
): number {
  const lastText = lastIndexWhere(rows, isAnswerText)
  if (lastText < 0) return rows.length
  return lastIndexWhere(
    rows,
    (row, index) => index < lastText && !isAnswerText(row, index),
  ) + 1
}

/**
 * The desktop attaches one footer to the terminal assistant part of each
 * settled turn. Its copy value is the complete visible answer, not merely the
 * final provider chunk, and its time is the turn completion time.
 */
export function assistantResponseFooters(session: AgentSession) {
  const footers = new Map<number, AssistantResponseFooter>()
  const turns = new Map(session.turns.map((turn) => [turn.id, turn]))
  const rowsByTurn = new Map<string, Array<number | null>>()
  const blocksByAnchor = new Map<number, AgentSession['transcript_blocks']>()

  for (const block of session.transcript_blocks) {
    if (!block.turn_id) continue
    const anchor = Math.min(block.after_message, session.messages.length)
    const blocks = blocksByAnchor.get(anchor) ?? []
    blocks.push(block)
    blocksByAnchor.set(anchor, blocks)
  }
  for (let index = 0; index <= session.messages.length; index++) {
    for (const block of blocksByAnchor.get(index) ?? []) {
      const rows = rowsByTurn.get(block.turn_id!) ?? []
      rows.push(null)
      rowsByTurn.set(block.turn_id!, rows)
    }
    const message = session.messages[index]
    if (message?.role === 'assistant' && message.turn_id) {
      const rows = rowsByTurn.get(message.turn_id) ?? []
      rows.push(index)
      rowsByTurn.set(message.turn_id, rows)
    }
  }

  for (const [turnId, rows] of rowsByTurn) {
    const turn = turns.get(turnId)
    if (turn?.status === 'running') continue
    const messageIndexes = rows.filter((index): index is number => index !== null)
    const footerIndex = messageIndexes.at(-1)
    if (footerIndex === undefined || session.messages[footerIndex]!.streaming) continue
    const answerStart = turnAnswerStart(rows, (index) => (
      index !== null && Boolean(session.messages[index]?.content.trim())
    ))
    const content = rows
      .slice(answerStart)
      .flatMap((index) => index === null ? [] : [session.messages[index]!])
      .filter((message) => message.content.trim())
      .map((message) => message.content)
      .join('\n\n')
    footers.set(footerIndex, {
      content,
      timestamp: turn?.completed_at ?? session.messages[footerIndex]!.created_at,
    })
  }

  session.messages.forEach((message, index) => {
    if (message.role === 'assistant' && !message.turn_id && !message.streaming) {
      footers.set(index, { content: message.content, timestamp: message.created_at })
    }
  })
  return footers
}

function localDateNumber(date: Date) {
  return Date.UTC(date.getFullYear(), date.getMonth(), date.getDate())
}

function sameLocalDate(left: Date, right: Date) {
  return left.getFullYear() === right.getFullYear()
    && left.getMonth() === right.getMonth()
    && left.getDate() === right.getDate()
}

function capitalize(value: string) {
  return value ? value[0]!.toLocaleUpperCase() + value.slice(1) : value
}

function language(locale?: Intl.LocalesArgument) {
  return new Intl.Locale(new Intl.DateTimeFormat(locale).resolvedOptions().locale).language
}

function ordinalSuffix(day: number) {
  if (day % 100 >= 11 && day % 100 <= 13) return 'th'
  if (day % 10 === 1) return 'st'
  if (day % 10 === 2) return 'nd'
  if (day % 10 === 3) return 'rd'
  return 'th'
}

function lastIndexWhere<T>(values: readonly T[], predicate: (value: T, index: number) => boolean) {
  for (let index = values.length - 1; index >= 0; index--) {
    if (predicate(values[index]!, index)) return index
  }
  return -1
}

function activityNoun(kind: ActivityItem['kind'], count: number) {
  const [one, many] = {
    reasoning: ['thought', 'thoughts'],
    command: ['command', 'commands'],
    fileChange: ['file edit', 'file edits'],
    fileRead: ['file read', 'file reads'],
    fileSearch: ['file search', 'file searches'],
    fileList: ['file list', 'file lists'],
    search: ['search', 'searches'],
    plan: ['plan step', 'plan steps'],
    tool: ['tool call', 'tool calls'],
  }[kind]
  return count === 1 ? one : many
}

function activityNounKey(kind: ActivityItem['kind'], count: number) {
  const [one, many] = {
    reasoning: ['activity.thought', 'activity.thoughts'],
    command: ['activity.command', 'activity.commands'],
    fileChange: ['activity.file_edit', 'activity.file_edits'],
    fileRead: ['activity.file_read', 'activity.file_reads'],
    fileSearch: ['activity.file_search', 'activity.file_searches'],
    fileList: ['activity.file_list', 'activity.file_lists'],
    search: ['activity.search', 'activity.searches'],
    plan: ['activity.plan_step', 'activity.plan_steps'],
    tool: ['activity.tool_call', 'activity.tool_calls'],
  }[kind]
  return count === 1 ? one : many
}

function pathName(path: string) {
  return path.split(/[\\/]/).filter(Boolean).at(-1) ?? path
}

function toolNameLeaf(name: string) {
  const namespaceLeaf = name.trim().split('__').at(-1) ?? name.trim()
  return namespaceLeaf.split(/[:./]/).at(-1) ?? namespaceLeaf
}

function isAskUserQuestion(activity: ActivityItem) {
  return activity.kind === 'tool'
    && toolNameLeaf(activity.title).replace(/[\s_-]+/g, '').toLocaleLowerCase() === 'askuserquestion'
}

function humanizeToolName(name: string) {
  const trimmed = name.trim()
  if (/\s/.test(trimmed)) return trimmed
  const display = toolNameLeaf(trimmed)
    .replace(/[_-]+/g, ' ')
    .replace(/([a-z\d])([A-Z])/g, '$1 $2')
    .replace(/([A-Z])([A-Z][a-z])/g, '$1 $2')
    .trim()
  return display ? display[0]!.toLocaleUpperCase() + display.slice(1) : 'Tool'
}

function activityToolDisplayName(activity: ActivityItem, t?: Translator) {
  if (isAskUserQuestion(activity)) {
    return t ? t('activity.ask_questions') : 'Ask questions'
  }
  const target = activity.display_target?.trim()
  if (target) return target
  if (!isGenericActivityTitle(activity)) return humanizeToolName(activity.title)
  return t ? t('activity.tool') : 'Tool'
}

function isGenericActivityTitle(activity: ActivityItem) {
  const normalized = activity.title.trim().toLocaleLowerCase().replace(/[\s_-]+/g, '')
  const generic = {
    command: ['command', 'runcommand', 'shell', 'shellcommand', 'execute', 'exec'],
    fileChange: ['filechange', 'editfile', 'writefile', 'patch', 'applypatch'],
    fileRead: ['fileread', 'readfile', 'read'],
    fileSearch: ['filesearch', 'searchfiles', 'findfiles', 'grep'],
    fileList: ['filelist', 'listfiles', 'listdirectory', 'ls'],
    search: ['search', 'websearch'],
    plan: ['plan', 'planupdated', 'updateplan'],
    tool: ['tool'],
    reasoning: ['reasoning'],
  }[activity.kind]
  return generic.includes(normalized)
}

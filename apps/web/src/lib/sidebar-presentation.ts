import type { AgentSession, Project } from '@waku/client'
import { projectDisplayName } from './project-presentation'

export type DateGroup = 'today' | 'yesterday' | 'week' | 'month' | 'year' | 'more'

export interface SessionItem {
  session: AgentSession
  projectName: string
  timestamp: number
}

export interface SessionGroup {
  id: DateGroup
  label: string
  sessions: SessionItem[]
}

export type SidebarListRow =
  | { kind: 'search'; key: 'search' }
  | { kind: 'group'; key: string; group: SessionGroup; collapsed: boolean; first: boolean }
  | { kind: 'session'; key: string; item: SessionItem }
  | { kind: 'spacer'; key: string }

const GROUP_LABELS: Record<DateGroup, string> = {
  today: 'Today',
  yesterday: 'Yesterday',
  week: 'This Week',
  month: 'This Month',
  year: 'This Year',
  more: 'More',
}

const GROUP_ORDER: DateGroup[] = ['today', 'yesterday', 'week', 'month', 'year', 'more']

export function sidebarRows(
  groups: SessionGroup[],
  collapsed: ReadonlySet<DateGroup>,
): SidebarListRow[] {
  const rows: SidebarListRow[] = [{ kind: 'search', key: 'search' }]
  const visibleGroups = groups.length
    ? groups
    // Desktop keeps the first header so Add Project never disappears merely
    // because there is no task history yet.
    : [{ id: 'today' as const, label: GROUP_LABELS.today, sessions: [] }]
  visibleGroups.forEach((group, index) => {
    const isCollapsed = collapsed.has(group.id)
    rows.push({
      kind: 'group',
      key: `group:${group.id}`,
      group,
      collapsed: isCollapsed,
      first: index === 0,
    })
    if (!isCollapsed) {
      rows.push(...group.sessions.map((item) => ({
        kind: 'session' as const,
        key: `session:${item.session.id}`,
        item,
      })))
    }
    if (groups.length) rows.push({ kind: 'spacer', key: `spacer:${group.id}` })
  })
  return rows
}

export function groupSessions(
  projects: Project[],
  sessions: AgentSession[],
  now = new Date(),
  unknownProject = 'Unknown project',
  projectlessName = 'No project',
): SessionGroup[] {
  const projectNames = new Map(projects.map((project) => [
    project.id,
    projectDisplayName(project, projectlessName),
  ]))
  const grouped = new Map<DateGroup, SessionItem[]>()
  const started = sessions
    .filter(sessionHasStarted)
    .sort((left, right) => sessionTimestamp(right) - sessionTimestamp(left))
  for (const session of started) {
    const id = dateGroup(sessionTimestamp(session), now)
    const items = grouped.get(id) ?? []
    items.push({
      session,
      projectName: projectNames.get(session.project_id) ?? unknownProject,
      timestamp: sessionTimestamp(session),
    })
    grouped.set(id, items)
  }
  return GROUP_ORDER
    .filter((id) => grouped.has(id))
    .map((id) => ({ id, label: GROUP_LABELS[id], sessions: grouped.get(id)! }))
}

export function sessionHasStarted(session: AgentSession): boolean {
  return Boolean(
    session.turns.length
      || session.messages.length
      || session.provider_cursor
      || session.last_reply_at,
  )
}

export function sessionTimeLabel(
  session: AgentSession,
  nowSeconds = Math.floor(Date.now() / 1_000),
  t?: Translator,
): string | null {
  const turn = session.turns.at(-1)
  if (
    (session.status === 'connecting' || session.status === 'working' || session.status === 'waiting')
      && turn?.status === 'running'
  ) {
    const elapsed = Math.max(0, nowSeconds - turn.started_at)
    return t
      ? t('sidebar.working', { elapsed: formatWorkingElapsedLocalized(elapsed, t) })
      : `Working for ${formatWorkingElapsed(elapsed)}`
  }
  if (session.last_reply_at == null) return null
  const elapsed = Math.max(0, nowSeconds - session.last_reply_at)
  return t ? formatTimeAgoLocalized(elapsed, t) : formatTimeAgo(elapsed)
}

export function nextSidebarUpdateDelay(
  sessions: AgentSession[],
  nowSeconds = Math.floor(Date.now() / 1_000),
): number {
  let next = secondsUntilLocalMidnight(nowSeconds)
  for (const session of sessions) {
    const turn = session.turns.at(-1)
    if (
      (session.status === 'connecting' || session.status === 'working' || session.status === 'waiting')
        && turn?.status === 'running'
    ) {
      return 1
    }
    if (session.last_reply_at == null) continue
    const elapsed = Math.max(0, nowSeconds - session.last_reply_at)
    const step = elapsed < 3_600 ? 60 : elapsed < 86_400 ? 3_600 : 86_400
    const remaining = Math.max(1, step - elapsed % step)
    next = Math.min(next, remaining)
  }
  return next
}

export function dateGroup(timestamp: number, now = new Date()): DateGroup {
  const date = new Date(timestamp * 1_000)
  const today = localDateStart(now)
  const sessionDay = localDateStart(date)
  if (sessionDay >= today) return 'today'

  const yesterday = new Date(today)
  yesterday.setDate(yesterday.getDate() - 1)
  if (sessionDay.getTime() === yesterday.getTime()) return 'yesterday'

  const weekStart = new Date(today)
  weekStart.setDate(weekStart.getDate() - ((weekStart.getDay() + 6) % 7))
  if (sessionDay >= weekStart) return 'week'

  if (
    sessionDay.getFullYear() === today.getFullYear()
      && sessionDay.getMonth() === today.getMonth()
  ) return 'month'
  if (sessionDay.getFullYear() === today.getFullYear()) return 'year'
  return 'more'
}

export function formatTimeAgo(seconds: number): string {
  if (seconds < 60) return 'just now'
  if (seconds < 3_600) return `${Math.floor(seconds / 60)}m`
  if (seconds < 86_400) return `${Math.floor(seconds / 3_600)}h`
  return `${Math.floor(seconds / 86_400)}d`
}

export function formatWorkingElapsed(seconds: number): string {
  if (seconds < 60) return `${seconds}s`
  if (seconds < 3_600) {
    const minutes = Math.floor(seconds / 60)
    const remainder = seconds % 60
    return remainder ? `${minutes}m ${remainder}s` : `${minutes}m`
  }
  const hours = Math.floor(seconds / 3_600)
  const minutes = Math.floor((seconds % 3_600) / 60)
  return minutes ? `${hours}h ${minutes}m` : `${hours}h`
}

function formatTimeAgoLocalized(seconds: number, t: Translator): string {
  if (seconds < 60) return t('sidebar.just_now')
  if (seconds < 3_600) return t('sidebar.minutes_ago', { count: Math.floor(seconds / 60) })
  if (seconds < 86_400) return t('sidebar.hours_ago', { count: Math.floor(seconds / 3_600) })
  return t('sidebar.days_ago', { count: Math.floor(seconds / 86_400) })
}

function formatWorkingElapsedLocalized(seconds: number, t: Translator): string {
  if (seconds < 60) return t('duration.seconds_short', { count: seconds })
  if (seconds < 3_600) {
    const minutes = Math.floor(seconds / 60)
    const remainder = seconds % 60
    const first = t('duration.minutes_short', { count: minutes })
    return remainder
      ? t('duration.two_units', { first, second: t('duration.seconds_short', { count: remainder }) })
      : first
  }
  const hours = Math.floor(seconds / 3_600)
  const minutes = Math.floor((seconds % 3_600) / 60)
  const first = t('duration.hours_short', { count: hours })
  return minutes
    ? t('duration.two_units', { first, second: t('duration.minutes_short', { count: minutes }) })
    : first
}

type Translator = (key: string, params?: Record<string, string | number>) => string

function sessionTimestamp(session: AgentSession): number {
  return session.last_reply_at ?? session.created_at
}

function localDateStart(date: Date): Date {
  return new Date(date.getFullYear(), date.getMonth(), date.getDate())
}

function secondsUntilLocalMidnight(nowSeconds: number): number {
  const now = new Date(nowSeconds * 1_000)
  const tomorrow = new Date(now.getFullYear(), now.getMonth(), now.getDate() + 1)
  return Math.max(1, Math.ceil((tomorrow.getTime() - now.getTime()) / 1_000))
}

import type { AgentSession, SessionMessageMatch } from '@waku/client'
import { useEffect, useRef, useState } from 'react'
import { ProviderIcon, WakuIcon, type WakuIconName } from '@/components/waku-icon'
import type { SettingsPageId } from '@/components/settings-view'
import { SETTINGS_PAGES } from '@/components/settings-view'
import { displayTitle, searchSessionMessages, type TaskState } from '@/lib/daemon-api'
import { useDaemon } from '@/lib/daemon-context'
import { useI18n } from '@/lib/i18n'
import { fuzzyScore, shouldKeepPreviousPaletteItems } from '@/lib/palette-search'
import { useMacLikePlatform } from '@/lib/platform'
import { projectDisplayName } from '@/lib/project-presentation'
import { sessionHasStarted } from '@/lib/sidebar-presentation'
import { cn } from '@/lib/utils'

type PaletteSection = 'suggested' | 'tasks' | 'commands' | 'settings'
type Translator = (key: string, params?: Record<string, string | number>) => string

interface PaletteItem {
  id: string
  section: PaletteSection
  label: string
  detail?: string
  content?: { source: string; snippet: string }
  icon?: WakuIconName
  provider?: AgentSession['provider']
  shortcut?: string
  keywords: string
  run: () => void
}

export interface CommandPaletteActions {
  newTask: () => void
  openProject: () => void
  chooseModel: () => void
  focusComposer: () => void
  toggleUsage: () => void
  toggleSidebar: () => void
  toggleRightPanel: () => void
  openSettings: (page: SettingsPageId) => void
  selectTask: (sessionId: string) => void
}

export function CommandPalette({
  open,
  taskState,
  selectedSessionId,
  sidebarVisible,
  rightPanelVisible,
  canChooseModel,
  canToggleUsage,
  actions,
  onOpenChange,
}: {
  open: boolean
  taskState: TaskState
  selectedSessionId?: string
  sidebarVisible: boolean
  rightPanelVisible: boolean
  canChooseModel: boolean
  canToggleUsage: boolean
  actions: CommandPaletteActions
  onOpenChange: (open: boolean) => void
}) {
  const { t } = useI18n()
  const { client } = useDaemon()
  const [query, setQuery] = useState('')
  const [matches, setMatches] = useState<SessionMessageMatch[]>([])
  const [matchesQuery, setMatchesQuery] = useState<string | null>(null)
  const [searchPending, setSearchPending] = useState(false)
  const [previousItems, setPreviousItems] = useState<PaletteItem[]>([])
  const [selected, setSelected] = useState(0)
  const input = useRef<HTMLInputElement>(null)
  const previousFocus = useRef<HTMLElement | null>(null)
  const messageSearchCache = useRef(new Map<string, SessionMessageMatch[]>())
  const macShortcuts = useMacLikePlatform()

  function restorePreviousFocus() {
    const previous = previousFocus.current
    previousFocus.current = null
    if (previous?.isConnected) previous.focus()
  }

  useEffect(() => {
    if (!open) {
      restorePreviousFocus()
      return
    }
    previousFocus.current = document.activeElement instanceof HTMLElement
      ? document.activeElement
      : null
    setQuery('')
    setMatches([])
    setMatchesQuery(null)
    setSearchPending(false)
    setPreviousItems([])
    setSelected(0)
    messageSearchCache.current.clear()
    requestAnimationFrame(() => input.current?.focus())
  }, [open])

  useEffect(() => {
    if (!open || !client || !query.trim()) {
      setMatchesQuery(null)
      setSearchPending(false)
      return
    }
    const normalized = query.trim()
    const cached = messageSearchCache.current.get(normalized)
    if (cached) {
      messageSearchCache.current.delete(normalized)
      messageSearchCache.current.set(normalized, cached)
      setMatches(cached)
      setMatchesQuery(normalized)
      setSearchPending(false)
      return
    }
    let current = true
    const timer = window.setTimeout(() => {
      void searchSessionMessages(client, normalized, 50)
        .then((next) => {
          if (!current) return
          messageSearchCache.current.set(normalized, next)
          while (messageSearchCache.current.size > 24) {
            const oldest = messageSearchCache.current.keys().next().value
            if (oldest === undefined) break
            messageSearchCache.current.delete(oldest)
          }
          setMatches(next)
          setMatchesQuery(normalized)
          setSearchPending(false)
        })
        .catch(() => {
          if (!current) return
          setMatches([])
          setMatchesQuery(normalized)
          setSearchPending(false)
        })
    }, 90)
    return () => {
      current = false
      window.clearTimeout(timer)
    }
  }, [client, open, query])

  const nextItems = buildItems({
    taskState,
    query,
    matches: matchesQuery === query.trim() ? matches : [],
    selectedSessionId,
    sidebarVisible,
    rightPanelVisible,
    canChooseModel,
    canToggleUsage,
    macShortcuts,
    actions,
    t,
  })
  const items = shouldKeepPreviousPaletteItems(
    nextItems.length,
    searchPending,
    previousItems.length,
  ) ? previousItems : nextItems

  useEffect(() => setSelected((current) => Math.min(current, Math.max(0, items.length - 1))), [items.length])
  if (!open) return null

  function execute(index = selected) {
    const item = items[index]
    if (!item) return
    restorePreviousFocus()
    onOpenChange(false)
    item.run()
  }

  function dismiss() {
    restorePreviousFocus()
    onOpenChange(false)
  }

  return (
    <div
      aria-label={t('menu.command_palette')}
      aria-modal="true"
      className="fixed inset-0 z-[100] flex items-start justify-center bg-black/14 px-6 pt-[clamp(48px,9vh,72px)] dark:bg-black/26"
      role="dialog"
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) dismiss()
      }}
    >
      <div className="flex max-h-[min(480px,calc(100dvh-108px))] w-full max-w-[680px] flex-col overflow-hidden rounded-[15px] bg-[var(--raised)] shadow-[0_24px_80px_rgba(0,0,0,0.26)]">
        <div className="flex h-[60px] shrink-0 items-center border-b px-[19px]">
          <input
            aria-activedescendant={items[selected] ? `palette-${items[selected]!.id}` : undefined}
            aria-controls="command-palette-results"
            aria-label={t('command_palette.placeholder')}
            autoComplete="off"
            className="h-full min-w-0 flex-1 bg-transparent text-[15.5px] outline-none placeholder:text-[var(--text-ghost)]"
            placeholder={t('command_palette.placeholder')}
            ref={input}
            role="combobox"
            value={query}
            onChange={(event) => {
              const nextQuery = event.target.value
              setPreviousItems(items)
              setQuery(nextQuery)
              setSearchPending(Boolean(client && nextQuery.trim()))
              setSelected(0)
            }}
            onKeyDown={(event) => {
              const page = event.key === 'PageDown' ? 7 : event.key === 'PageUp' ? -7 : 0
              if (event.key === 'ArrowDown' || (event.ctrlKey && event.key === 'n') || event.key === 'Tab' && !event.shiftKey) {
                event.preventDefault()
                setSelected((current) => items.length ? (current + 1) % items.length : 0)
              } else if (event.key === 'ArrowUp' || (event.ctrlKey && event.key === 'p') || event.key === 'Tab' && event.shiftKey) {
                event.preventDefault()
                setSelected((current) => items.length ? (current - 1 + items.length) % items.length : 0)
              } else if (event.key === 'Home') {
                event.preventDefault()
                setSelected(0)
              } else if (event.key === 'End') {
                event.preventDefault()
                setSelected(Math.max(0, items.length - 1))
              } else if (page) {
                event.preventDefault()
                setSelected((current) => Math.max(0, Math.min(items.length - 1, current + page)))
              } else if (event.key === 'Enter') {
                event.preventDefault()
                execute()
              } else if (event.key === 'Escape') {
                event.preventDefault()
                dismiss()
              }
            }}
          />
        </div>
        <div className="min-h-0 overflow-y-auto px-2 pb-2" id="command-palette-results" role="listbox">
          {!items.length && !searchPending ? (
            <div className="grid h-[180px] place-items-center text-center">
              <div>
                <WakuIcon className="mx-auto size-[18px] text-[var(--text-ghost)]" name="search" />
                <div className="mt-3 text-[13px] font-medium text-[var(--text-secondary)]">{t('command_palette.no_results')}</div>
                <div className="mt-[5px] text-[11.5px] text-[var(--text-tertiary)]">{t('command_palette.no_results_hint')}</div>
              </div>
            </div>
          ) : (
            <PaletteRows items={items} query={query} selected={selected} t={t} onExecute={execute} onSelected={setSelected} />
          )}
        </div>
      </div>
    </div>
  )
}

function PaletteRows({
  items,
  query,
  selected,
  t,
  onSelected,
  onExecute,
}: {
  items: PaletteItem[]
  query: string
  selected: number
  t: Translator
  onSelected: (index: number) => void
  onExecute: (index: number) => void
}) {
  let section: PaletteSection | null = null
  return items.map((item, index) => {
    const header = section !== item.section
    section = item.section
    return (
      <div key={item.id}>
        {header && (
          <div className="flex h-[30px] items-center px-[9px] pt-2.5 text-[11px] font-medium text-[var(--text-tertiary)]">
            {t(`command_palette.${item.section}`)}
          </div>
        )}
        <button
          aria-selected={index === selected}
          className={cn(
            'flex w-full items-center gap-2.5 rounded-[9px] border border-transparent px-[11px] text-left outline-none hover:bg-accent',
            item.content ? 'h-[60px]' : 'h-11',
            index === selected && 'border-input bg-accent',
          )}
          id={`palette-${item.id}`}
          role="option"
          type="button"
          onClick={() => onExecute(index)}
          onMouseEnter={() => onSelected(index)}
        >
          <span className="grid size-5 shrink-0 place-items-center text-[var(--text-secondary)]">
            {item.provider
              ? <ProviderIcon className="size-4" provider={item.provider} />
              : item.icon && <WakuIcon className="size-4" name={item.icon} />}
          </span>
          <span className="min-w-0 flex-1">
            <span className="flex min-w-0 items-baseline gap-[7px]">
              <span className={cn('truncate text-[14px] text-[var(--text-secondary)]', index === selected && 'font-medium text-foreground')}>{item.label}</span>
              {item.detail && <span className="truncate text-[11.5px] text-[var(--text-tertiary)]">{item.detail}</span>}
            </span>
            {item.content && (
              <span className="mt-0.5 block truncate text-[11.5px] text-[var(--text-tertiary)]">
                <span className="font-medium text-[var(--text-secondary)]">{item.content.source}: </span>
                <Highlighted text={item.content.snippet} query={query} />
              </span>
            )}
          </span>
          {item.shortcut && (
            <kbd className="flex h-[22px] min-w-7 shrink-0 items-center justify-center rounded-[7px] bg-[color:var(--foreground)]/[0.07] px-[7px] font-sans text-[11.5px] text-[var(--text-tertiary)]">
              {item.shortcut}
            </kbd>
          )}
        </button>
      </div>
    )
  })
}

function Highlighted({ text, query }: { text: string; query: string }) {
  const normalized = query.trim()
  if (!normalized) return text
  const at = text.toLowerCase().indexOf(normalized.toLowerCase())
  if (at < 0) return text
  return <>{text.slice(0, at)}<mark className="bg-transparent font-medium text-foreground">{text.slice(at, at + normalized.length)}</mark>{text.slice(at + normalized.length)}</>
}

function buildItems({
  taskState,
  query,
  matches,
  selectedSessionId,
  sidebarVisible,
  rightPanelVisible,
  canChooseModel,
  canToggleUsage,
  macShortcuts,
  actions,
  t,
}: {
  taskState: TaskState
  query: string
  matches: SessionMessageMatch[]
  selectedSessionId?: string
  sidebarVisible: boolean
  rightPanelVisible: boolean
  canChooseModel: boolean
  canToggleUsage: boolean
  macShortcuts: boolean
  actions: CommandPaletteActions
  t: Translator
}) {
  const searching = Boolean(query.trim())
  const commandSection: PaletteSection = searching ? 'commands' : 'suggested'
  const shortcut = (mac: string, other: string) => macShortcuts ? mac : other
  const commands: PaletteItem[] = [
    command('new-task', commandSection, t('command_palette.new_task'), 'pencil', shortcut('⌘N', 'Ctrl+N'), `new task session chat conversation start ${t('command_palette.new_task')}`, actions.newTask),
    command('open-project', commandSection, t('command_palette.open_project'), 'folder', shortcut('⌘O', 'Ctrl+O'), `open add folder project workspace repository repo ${t('command_palette.open_project')}`, actions.openProject),
  ]
  if (canChooseModel) commands.push(command('choose-model', commandSection, t('command_palette.choose_model'), 'bot', shortcut('⌘/', 'Ctrl+/'), `choose change select model provider agent ${t('command_palette.choose_model')}`, actions.chooseModel))
  if (searching) {
    commands.push(command('focus-composer', 'commands', t('menu.focus_composer'), 'pencil', shortcut('⌘L', 'Ctrl+L'), `focus composer prompt input message ${t('menu.focus_composer')}`, actions.focusComposer))
    if (canToggleUsage) {
      commands.push(command('toggle-usage', 'commands', t('menu.toggle_usage_panel'), 'gauge', shortcut('⌘U', 'Ctrl+U'), `toggle usage limits rate quota panel ${t('menu.toggle_usage_panel')}`, actions.toggleUsage))
    }
    commands.push(
      command('toggle-sidebar', 'commands', t(sidebarVisible ? 'command_palette.hide_sidebar' : 'command_palette.show_sidebar'), 'panelLeft', shortcut('⌘B', 'Ctrl+B'), 'toggle show hide left sidebar history tasks', actions.toggleSidebar),
      command('toggle-right-panel', 'commands', t(rightPanelVisible ? 'command_palette.hide_right_panel' : 'command_palette.show_right_panel'), 'panelRight', shortcut('⇧⌘B', 'Ctrl+Shift+B'), 'toggle show hide right panel files review diff terminal', actions.toggleRightPanel),
    )
  }
  for (const page of SETTINGS_PAGES) {
    commands.push(command(
      `settings-${page.id}`,
      'settings',
      t(page.labelKey),
      page.icon,
      page.id === 'general' ? shortcut('⌘,', 'Ctrl+,') : undefined,
      `${page.keywords} ${t(page.keywordsKey)}`,
      () => actions.openSettings(page.id),
    ))
  }

  if (!searching) return commands
  const matchBySession = new Map(matches.map((match) => [match.session_id, match]))
  const projectById = new Map(taskState.projects.map((project) => [project.id, project]))
  const tasks = taskState.sessions
    .filter(sessionHasStarted)
    .map((session, order) => {
      const project = projectById.get(session.project_id)
      const projectName = project
        ? projectDisplayName(project, t('project.no_project_name'))
        : t('sidebar.unknown_project')
      const match = matchBySession.get(session.id)
      const branch = session.workspace?.kind === 'worktree' ? session.workspace.branch : null
      const detail = [projectName, branch ? `#${branch}` : null, session.id === selectedSessionId ? t('command_palette.current') : null].filter(Boolean).join(' · ')
      const keywords = `${displayTitle(session)} ${project?.name ?? ''} ${project?.path ?? ''} ${branch ?? ''} ${session.provider} ${session.model ?? ''} task session chat conversation`
      const metadataScore = fuzzyScore(query, keywords)
      const contentScore = match ? fuzzyScore(query, match.snippet) ?? 0 : null
      const score = Math.max(metadataScore ?? -1, contentScore ?? -1)
      return score < 0 ? null : {
        id: `task-${session.id}`,
        section: 'tasks' as const,
        label: displayTitle(session),
        detail,
        content: match ? { source: t(match.source === 'user' ? 'command_palette.you' : 'command_palette.agent'), snippet: match.snippet } : undefined,
        provider: session.provider,
        keywords,
        run: () => actions.selectTask(session.id),
        recency: session.updated_at,
        order,
        score,
      }
    })
    .filter((item): item is NonNullable<typeof item> => item !== null)
    .sort((left, right) => right.score - left.score
      || right.recency - left.recency
      || left.order - right.order)
    .slice(0, 12)
    .map(({ recency: _, order: __, score: ___, ...item }) => item)

  const sectionRank: Record<PaletteSection, number> = {
    tasks: 0,
    commands: 1,
    suggested: 1,
    settings: 2,
  }
  const matchingCommands = commands
    .map((item, order) => ({
      item,
      order,
      score: fuzzyScore(query, `${item.label} ${item.keywords}`),
    }))
    .filter((scored): scored is typeof scored & { score: number } => scored.score !== null)
    .sort((left, right) => sectionRank[left.item.section] - sectionRank[right.item.section]
      || right.score - left.score
      || left.order - right.order)
    .map(({ item }) => item)

  return [
    ...tasks,
    ...matchingCommands,
  ]
}

function command(
  id: string,
  section: PaletteSection,
  label: string,
  icon: WakuIconName,
  shortcut: string | undefined,
  keywords: string,
  run: () => void,
): PaletteItem {
  return { id, section, label, icon, shortcut, keywords, run }
}

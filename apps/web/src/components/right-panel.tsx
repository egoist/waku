import { keepPreviousData, useQuery, useQueryClient } from '@tanstack/react-query'
import type { Editor } from '@pierre/diffs/edit'
import type { AgentSession, Project, ReviewDiffSource, WorkingTreeEntry } from '@waku/client'
import { GhosttyCore } from '@wterm/ghostty'
import { Terminal, useTerminal } from '@wterm/react'
import {
  lazy,
  Suspense,
  useCallback,
  useEffect,
  useRef,
  useState,
  type Dispatch,
  type KeyboardEvent as ReactKeyboardEvent,
  type ReactNode,
  type SetStateAction,
} from 'react'
import { Virtuoso, type VirtuosoHandle } from 'react-virtuoso'
import { toast } from 'sonner'
import { ControlMenu } from '@/components/control-menu'
import { PanelResizeHandle } from '@/components/panel-resize-handle'
import { Button } from '@/components/ui/button'
import { FileTypeIcon, WakuIcon, type WakuIconName } from '@/components/waku-icon'
import type { CodeDiffSurfaceHandle, DiffSurfaceFile } from '@/components/code-surfaces'
import {
  collectWorkspaceDiff,
  daemonKeys,
  listWorkspaceTree,
  readWorkspaceTextFile,
  sessionCwd,
  writeWorkspaceTextFile,
} from '@/lib/daemon-api'
import { useDaemon } from '@/lib/daemon-context'
import { useI18n } from '@/lib/i18n'
import { usePrimaryShortcut } from '@/lib/platform'
import { projectDisplayName } from '@/lib/project-presentation'
import {
  latestReviewTurnSource,
  reviewDiffSourceLabel,
  sameReviewDiffSource,
} from '@/lib/review-diff'
import {
  mergeReviewDiffFiles,
  openFileInPanel,
  tabNavigationIndex,
  treeNavigationAction,
  type TabNavigationKey,
  type TreeNavigationKey,
} from '@/lib/right-panel-state'
import { formatWorkingElapsed, type Translator } from '@/lib/transcript-presentation'
import {
  useRuntime,
  type BackgroundWorkItem,
  type BackgroundWorkKey,
  type BackgroundWorkStatus,
} from '@/lib/runtime-context'
import { cn } from '@/lib/utils'

const CodeFileSurface = lazy(() => import('@/components/code-surfaces').then((module) => ({
  default: module.CodeFileSurface,
})))
const CodeDiffSurface = lazy(() => import('@/components/code-surfaces').then((module) => ({
  default: module.CodeDiffSurface,
})))

export type PanelSurface = 'files' | 'file' | 'changes' | 'terminal' | 'backgroundWork'

interface PanelTab {
  id: string
  surface: PanelSurface
  terminalId?: string
  title?: string
  selectedFile?: string | null
  dirty?: boolean
  backgroundWorkKey?: BackgroundWorkKey
}

interface PanelState {
  tabs: PanelTab[]
  activeId: string | null
}

export function RightPanel({
  active,
  open,
  panelWidth,
  session,
  project,
  requestedSurface,
  requestedDiffSource,
  requestedBackgroundWorkKey,
  requestedFile,
  requestSignal,
  sidebarWidth,
  onOpenChange,
  onPanelWidthChange,
}: {
  active: boolean
  open: boolean
  panelWidth: number
  session: AgentSession | null
  project?: Project
  requestedSurface: PanelSurface
  requestedDiffSource: ReviewDiffSource
  requestedBackgroundWorkKey: BackgroundWorkKey | null
  requestedFile: string | null
  requestSignal: number
  sidebarWidth: number
  onOpenChange: (open: boolean) => void
  onPanelWidthChange: Dispatch<SetStateAction<number>>
}) {
  const { t } = useI18n()
  const [{ tabs, activeId }, setPanelState] = useState<PanelState>({
    tabs: [],
    activeId: null,
  })
  const [fileBuffers, setFileBuffers] = useState<Record<string, FileBuffer>>({})
  const [diffSource, setDiffSource] = useState<ReviewDiffSource>('uncommitted')
  const tabStrip = useRef<HTMLDivElement>(null)
  const [tabOverflow, setTabOverflow] = useState({ start: false, end: false })
  const viewportWidth = useViewportWidth()
  const maxPanelWidth = Math.max(280, Math.min(1_000, viewportWidth - sidebarWidth - 360))
  const fittedPanelWidth = clamp(panelWidth, 280, maxPanelWidth)
  const bufferRoot = session && project ? sessionCwd(session, project) : undefined

  useEffect(() => {
    setFileBuffers({})
  }, [bufferRoot])

  const openSurface = useCallback((
    surface: PanelSurface,
    source: ReviewDiffSource = 'uncommitted',
    backgroundWorkKey?: BackgroundWorkKey | null,
    file?: string | null,
  ) => {
    if (surface === 'changes') setDiffSource(source)
    if (surface === 'changes') onPanelWidthChange((current) => Math.max(current, 820))
    if (surface === 'files' && file) {
      onPanelWidthChange((current) => Math.max(current, 684))
      setPanelState((current) => openFileInPanel(
        current,
        file,
        undefined,
        () => ({
          id: crypto.randomUUID(),
          surface: 'file',
          selectedFile: file,
        }),
      ))
      return
    }
    setPanelState((current) => {
      const reusable = surface === 'terminal'
        ? undefined
        : current.tabs.find((tab) => tab.surface === surface && (
            surface === 'file'
              ? tab.selectedFile === file
              : surface !== 'backgroundWork'
                || (backgroundWorkKey && tab.backgroundWorkKey
                  && sameBackgroundWorkKey(backgroundWorkKey, tab.backgroundWorkKey))
          ))
      if (reusable) {
        return {
          ...current,
          activeId: reusable.id,
          tabs: (surface === 'files' || surface === 'file') && file
            ? current.tabs.map((tab) => tab.id === reusable.id
              ? { ...tab, selectedFile: file }
              : tab)
            : current.tabs,
        }
      }
      if (surface === 'backgroundWork' && !backgroundWorkKey) return current
      const id = crypto.randomUUID()
      const tab: PanelTab = surface === 'terminal'
        ? { id, surface, terminalId: crypto.randomUUID() }
        : {
            id,
            surface,
            selectedFile: surface === 'files' || surface === 'file' ? file ?? null : undefined,
            backgroundWorkKey: surface === 'backgroundWork'
              ? backgroundWorkKey ?? undefined
              : undefined,
          }
      return { tabs: [...current.tabs, tab], activeId: id }
    })
  }, [onPanelWidthChange])

  useEffect(() => {
    if (requestSignal > 0) {
      openSurface(requestedSurface, requestedDiffSource, requestedBackgroundWorkKey, requestedFile)
    }
  }, [openSurface, requestedBackgroundWorkKey, requestedDiffSource, requestedFile, requestedSurface, requestSignal])

  const activeTab = tabs.find((tab) => tab.id === activeId) ?? tabs.at(-1)

  const updateTabOverflow = useCallback(() => {
    const strip = tabStrip.current
    if (!strip) return
    const next = {
      start: strip.scrollLeft > 1,
      end: strip.scrollLeft + strip.clientWidth < strip.scrollWidth - 1,
    }
    setTabOverflow((current) => current.start === next.start && current.end === next.end
      ? current
      : next)
  }, [])

  useEffect(() => {
    const strip = tabStrip.current
    if (!strip) return
    updateTabOverflow()
    const resize = new ResizeObserver(updateTabOverflow)
    resize.observe(strip)
    return () => resize.disconnect()
  }, [tabs, updateTabOverflow])

  useEffect(() => {
    if (!activeTab) return
    const frame = window.requestAnimationFrame(() => {
      const strip = tabStrip.current
      const tab = document.getElementById(panelTabId(activeTab.id))
      if (!strip || !tab) return
      const stripBounds = strip.getBoundingClientRect()
      const tabBounds = tab.getBoundingClientRect()
      const inset = 16
      if (tabBounds.left < stripBounds.left + inset) {
        strip.scrollLeft -= stripBounds.left + inset - tabBounds.left
      } else if (tabBounds.right > stripBounds.right - inset) {
        strip.scrollLeft += tabBounds.right - (stripBounds.right - inset)
      }
      updateTabOverflow()
    })
    return () => window.cancelAnimationFrame(frame)
  }, [activeTab, updateTabOverflow])

  function closeTab(tabId: string) {
    const index = tabs.findIndex((tab) => tab.id === tabId)
    if (index < 0) return
    const remaining = tabs.filter((tab) => tab.id !== tabId)
    setPanelState({
      tabs: remaining,
      activeId: activeId === tabId
        ? remaining[Math.min(index, remaining.length - 1)]?.id ?? null
        : activeId,
    })
    if (remaining.length === 0) onOpenChange(false)
  }

  function focusTab(index: number) {
    const tab = tabs[index]
    if (!tab) return
    setPanelState((current) => ({ ...current, activeId: tab.id }))
    window.requestAnimationFrame(() => {
      document.getElementById(panelTabId(tab.id))?.focus()
    })
  }

  const updateTab = useCallback((tabId: string, update: Partial<PanelTab>) => {
    setPanelState((current) => ({
      ...current,
      tabs: current.tabs.map((tab) => tab.id === tabId ? { ...tab, ...update } : tab),
    }))
  }, [])

  const openFile = useCallback((tabId: string, selectedFile: string, treeWidth: number) => {
    onPanelWidthChange((current) => Math.max(current, treeWidth + 500))
    setPanelState((current) => openFileInPanel(
      current,
      selectedFile,
      tabId,
      () => ({
        id: crypto.randomUUID(),
        surface: 'file',
        selectedFile,
      }),
    ))
  }, [onPanelWidthChange])

  const setTabDirty = useCallback((tabId: string, dirty: boolean) => {
    setPanelState((current) => {
      const tab = current.tabs.find((candidate) => candidate.id === tabId)
      if (!tab || Boolean(tab.dirty) === dirty) return current
      return {
        ...current,
        tabs: current.tabs.map((candidate) => candidate.id === tabId
          ? { ...candidate, dirty }
          : candidate),
      }
    })
  }, [])

  return (
    <aside
      className={cn(
        'absolute inset-y-0 right-0 z-30 flex shrink-0 flex-col border-l bg-background shadow-2xl xl:relative xl:z-auto xl:shadow-none',
        !open && 'hidden',
      )}
      style={{ width: `min(${fittedPanelWidth}px, 92vw)` }}
    >
      <PanelResizeHandle
        edge="left"
        label={t('right_panel.resize')}
        max={maxPanelWidth}
        min={280}
        value={fittedPanelWidth}
        onChange={onPanelWidthChange}
      />
      <header className="flex h-12 shrink-0 items-center gap-1.5 px-2.5 pr-3.5">
        <div className="relative min-w-0 flex-1 self-stretch overflow-hidden">
          <div
            aria-label={t('right_panel.tabs')}
            className="flex h-full min-w-0 items-center gap-1 overflow-x-auto overscroll-x-contain"
            ref={tabStrip}
            role="tablist"
            onScroll={updateTabOverflow}
          >
            {tabs.map((tab, index) => (
              <PanelTabButton
                active={tab.id === activeTab?.id}
                key={tab.id}
                tab={tab}
                onActivate={() => setPanelState((current) => ({ ...current, activeId: tab.id }))}
                onClose={() => closeTab(tab.id)}
                onNavigate={(key) => focusTab(tabNavigationIndex(tabs.length, index, key))}
              />
            ))}
            <span aria-hidden="true" className="h-px w-4 shrink-0" />
          </div>
          <span
            aria-hidden="true"
            className={cn(
              'pointer-events-none absolute inset-y-0 left-0 w-4 bg-gradient-to-r from-background to-transparent transition-opacity motion-reduce:transition-none',
              tabOverflow.start ? 'opacity-100' : 'opacity-0',
            )}
          />
          <span
            aria-hidden="true"
            className={cn(
              'pointer-events-none absolute inset-y-0 right-0 w-4 bg-gradient-to-l from-background to-transparent transition-opacity motion-reduce:transition-none',
              tabOverflow.end ? 'opacity-100' : 'opacity-0',
            )}
          />
        </div>
        {tabs.length > 0 && (
          <ControlMenu
            align="right"
            caret={false}
            highlightTriggerWhenOpen={false}
            label={t('right_panel.add_tab')}
            placement="below"
            selectionMode="status"
            triggerClassName="size-7 justify-center px-0"
            items={[
              {
                id: 'terminal',
                label: t('right_panel.terminal'),
                icon: 'terminal',
                onSelect: () => openSurface('terminal'),
              },
              {
                id: 'files',
                label: t('right_panel.files'),
                icon: 'folder',
                selected: tabs.some((tab) => tab.surface === 'files'),
                onSelect: () => openSurface('files'),
              },
              {
                id: 'changes',
                label: t('right_panel.diff'),
                icon: 'fileDiff',
                selected: tabs.some((tab) => tab.surface === 'changes'),
                onSelect: () => openSurface('changes'),
              },
            ]}
          >
            <WakuIcon className="size-3.5" name="plus" />
          </ControlMenu>
        )}
        <Button aria-label={t('right_panel.hide')} size="icon-sm" variant="ghost" onClick={() => onOpenChange(false)}>
          <WakuIcon name="panelRight" />
        </Button>
      </header>

      {!activeTab && <PanelChooser onSelect={openSurface} />}
      {tabs.map((tab) => (
        <div
          aria-labelledby={panelTabId(tab.id)}
          className={cn('min-h-0 flex-1', tab.id === activeTab?.id ? 'flex' : 'hidden')}
          id={panelContentId(tab.id)}
          key={tab.id}
          role="tabpanel"
        >
          {(tab.surface === 'files' || tab.surface === 'file') && (
            <FilesPanel
              active={active && tab.id === activeTab?.id}
              buffers={fileBuffers}
              panelWidth={fittedPanelWidth}
              project={project}
              requestedFile={tab.selectedFile ?? null}
              session={session}
              setBuffers={setFileBuffers}
              tabId={tab.id}
              onDirtyChange={setTabDirty}
              onOpenFile={openFile}
            />
          )}
          {tab.surface === 'changes' && (
            <ChangesPanel
              diffSource={diffSource}
              panelWidth={fittedPanelWidth}
              project={project}
              session={session}
              onDiffSourceChange={setDiffSource}
            />
          )}
          {tab.surface === 'terminal' && tab.terminalId && (
            <TerminalPanel
              project={project}
              session={session}
              terminalId={tab.terminalId}
              onTitle={(title) => updateTab(tab.id, { title })}
            />
          )}
          {tab.surface === 'backgroundWork' && tab.backgroundWorkKey && (
            <BackgroundWorkPanel
              session={session}
              workKey={tab.backgroundWorkKey}
              onTitle={(title) => updateTab(tab.id, { title })}
            />
          )}
        </div>
      ))}
    </aside>
  )
}

function PanelTabButton({
  tab,
  active,
  onActivate,
  onClose,
  onNavigate,
}: {
  tab: PanelTab
  active: boolean
  onActivate: () => void
  onClose: () => void
  onNavigate: (key: TabNavigationKey) => void
}) {
  const { t } = useI18n()
  const saveShortcut = usePrimaryShortcut('⌘S', 'Ctrl+S')
  const title = tab.surface === 'files' || tab.surface === 'file'
    ? tab.selectedFile?.split('/').at(-1) ?? t('right_panel.files')
    : tab.surface === 'changes'
      ? t('right_panel.diff')
      : tab.surface === 'backgroundWork'
        ? tab.title?.trim() || backgroundWorkKindLabel(tab.backgroundWorkKey?.kind, t)
        : tab.title?.trim() || t('right_panel.terminal')
  const icon = tab.surface === 'files' || tab.surface === 'file'
    ? 'folder'
    : tab.surface === 'changes'
      ? 'fileDiff'
      : tab.surface === 'backgroundWork'
        ? backgroundWorkKindIcon(tab.backgroundWorkKey?.kind)
        : 'terminal'
  return (
    <div
      aria-controls={panelContentId(tab.id)}
      aria-selected={active}
      className={cn(
        'flex h-7 min-w-[100px] max-w-44 shrink-0 items-center gap-1.5 rounded-md px-2 text-left text-[12px] outline-none hover:bg-accent focus-visible:ring-1 focus-visible:ring-ring',
        active ? 'bg-accent text-foreground' : 'text-[var(--text-secondary)]',
      )}
      id={panelTabId(tab.id)}
      role="tab"
      tabIndex={active ? 0 : -1}
      onClick={onActivate}
      onKeyDown={(event) => {
        if (event.target !== event.currentTarget) return
        if (event.key === 'Enter' || event.key === ' ') {
          event.preventDefault()
          onActivate()
        } else if (['ArrowLeft', 'ArrowRight', 'Home', 'End'].includes(event.key)) {
          event.preventDefault()
          onNavigate(event.key as TabNavigationKey)
        }
      }}
    >
      {(tab.surface === 'files' || tab.surface === 'file') && tab.selectedFile
        ? <FileTypeIcon className="size-[13px]" path={tab.selectedFile} />
        : <WakuIcon className="size-[13px] text-[var(--text-secondary)]" name={icon} />}
      <span className="min-w-0 flex-1 truncate">{title}</span>
      {tab.dirty && (
        <span
          aria-label={t('files.unsaved_changes', { shortcut: saveShortcut })}
          className="size-[7px] shrink-0 rounded-full bg-[var(--warning)]"
          role="status"
          title={t('files.unsaved_changes', { shortcut: saveShortcut })}
        />
      )}
      <button
        aria-label={t('right_panel.close_tab', { title })}
        className="grid size-4 shrink-0 place-items-center rounded hover:bg-accent"
        tabIndex={active ? 0 : -1}
        type="button"
        onClick={(event) => {
          event.stopPropagation()
          onClose()
        }}
      >
        <WakuIcon className="size-2.5 text-[var(--text-tertiary)]" name="x" />
      </button>
    </div>
  )
}

function PanelChooser({ onSelect }: { onSelect: (surface: PanelSurface) => void }) {
  const { t } = useI18n()
  return (
    <div className="flex min-h-0 flex-1 items-center justify-center px-5 pb-8">
      <div className="w-full max-w-[420px] text-center">
        <h3 className="text-[13px] font-medium">{t('right_panel.open_surface')}</h3>
        <p className="mt-[5px] text-[11px] text-[var(--text-tertiary)]">{t('right_panel.choose_surface')}</p>
        <div className="mt-5 grid grid-cols-2 gap-2 text-left">
          <PanelCard icon={<WakuIcon className="size-[18px]" name="terminal" />} label={t('right_panel.terminal')} description={t('right_panel.terminal_description')} onClick={() => onSelect('terminal')} />
          <PanelCard icon={<WakuIcon className="size-[18px]" name="folder" />} label={t('right_panel.files')} description={t('right_panel.files_description')} onClick={() => onSelect('files')} />
          <PanelCard icon={<WakuIcon className="size-[18px]" name="fileDiff" />} label={t('right_panel.diff')} description={t('right_panel.diff_description')} onClick={() => onSelect('changes')} />
        </div>
      </div>
    </div>
  )
}

function PanelCard({
  icon,
  label,
  description,
  onClick,
}: {
  icon: ReactNode
  label: string
  description: string
  onClick?: () => void
}) {
  return (
    <button
      className="flex h-28 min-w-0 flex-col rounded-lg border border-[var(--input)] bg-card p-3.5 text-left outline-none hover:border-[var(--text-ghost)] hover:bg-[var(--raised)] active:bg-accent focus-visible:ring-1 focus-visible:ring-ring"
      type="button"
      onClick={onClick}
    >
      <span className="text-[var(--text-tertiary)]">{icon}</span>
      <span className="mt-3 text-[12.5px] font-medium">{label}</span>
      <span className="mt-1 text-[10.5px] leading-4 text-[var(--text-tertiary)]">{description}</span>
    </button>
  )
}

interface FileBuffer {
  content: string
  diskContent: string
  revision: number
  saving: boolean
  editor?: Editor<undefined>
}

function FilesPanel({
  active,
  buffers,
  tabId,
  session,
  project,
  requestedFile,
  panelWidth,
  setBuffers,
  onDirtyChange,
  onOpenFile,
}: {
  active: boolean
  buffers: Record<string, FileBuffer>
  tabId: string
  session: AgentSession | null
  project?: Project
  requestedFile: string | null
  panelWidth: number
  setBuffers: Dispatch<SetStateAction<Record<string, FileBuffer>>>
  onDirtyChange: (tabId: string, dirty: boolean) => void
  onOpenFile: (tabId: string, path: string, treeWidth: number) => void
}) {
  const { t } = useI18n()
  const { client, config, phase } = useDaemon()
  const queryClient = useQueryClient()
  const [expanded, setExpanded] = useState<string[]>([])
  const [selected, setSelected] = useState<string | null>(null)
  const [focusedTreeEntry, setFocusedTreeEntry] = useState<string | null>(null)
  const treeList = useRef<VirtuosoHandle>(null)
  const buffersRef = useRef(buffers)
  buffersRef.current = buffers
  const [treeWidth, setTreeWidth] = useState(() => readStoredWidth('waku.fileTreeWidth', 184, 140, 360))
  const treeWidthRef = useRef(treeWidth)
  treeWidthRef.current = treeWidth
  const root = session && project ? sessionCwd(session, project) : undefined
  const maxTreeWidth = Math.max(140, Math.min(360, panelWidth - 140))
  const fittedTreeWidth = clamp(treeWidth, 140, maxTreeWidth)
  const previousRoot = useRef(root)

  useEffect(() => {
    if (previousRoot.current === root) return
    previousRoot.current = root
    setExpanded([])
    setSelected(requestedFile)
    setFocusedTreeEntry(null)
  }, [requestedFile, root])

  useEffect(() => {
    if (!requestedFile || requestedFile === selected) return
    setSelected(requestedFile)
    setExpanded((current) => {
      const next = new Set(current)
      for (const path of absoluteParentPaths(root, requestedFile)) next.add(path)
      return [...next]
    })
  }, [requestedFile, root, selected])

  useEffect(() => {
    const timer = window.setTimeout(() => {
      window.localStorage.setItem('waku.fileTreeWidth', String(Math.round(treeWidth)))
    }, 150)
    return () => window.clearTimeout(timer)
  }, [treeWidth])

  const tree = useQuery({
    queryKey: daemonKeys.workspaceTree(config?.address ?? 'disconnected', root ?? 'none', expanded),
    queryFn: () => listWorkspaceTree(requireClient(client), root!, expanded),
    enabled: phase === 'connected' && Boolean(client && config && root),
    placeholderData: keepPreviousData,
  })
  const file = useQuery({
    queryKey: daemonKeys.workspaceFile(config?.address ?? 'disconnected', root ?? 'none', selected ?? 'none'),
    queryFn: () => readWorkspaceTextFile(requireClient(client), root!, selected!),
    enabled: phase === 'connected' && Boolean(client && config && root && selected),
  })
  const refetchFile = file.refetch

  useEffect(() => {
    if (!selected || file.data === undefined) return
    setBuffers((current) => {
      const buffer = current[selected]
      if (buffer && buffer.content !== buffer.diskContent) return current
      if (buffer?.diskContent === file.data) return current
      return {
        ...current,
        [selected]: {
          content: file.data,
          diskContent: file.data,
          revision: (buffer?.revision ?? -1) + 1,
          saving: false,
          editor: buffer?.editor,
        },
      }
    })
  }, [file.data, selected])

  const selectedBuffer = selected ? buffers[selected] : undefined
  const dirty = Boolean(selectedBuffer && selectedBuffer.content !== selectedBuffer.diskContent)
  useEffect(() => {
    onDirtyChange(tabId, dirty)
  }, [dirty, onDirtyChange, tabId])

  const saveSelected = useCallback(async () => {
    if (!selected || !root || !client || !config) return
    const buffer = buffersRef.current[selected]
    if (!buffer || buffer.saving || buffer.content === buffer.diskContent) return
    const path = selected
    const snapshot = buffer.content
    setBuffers((current) => current[path]
      ? { ...current, [path]: { ...current[path], saving: true } }
      : current)
    try {
      await writeWorkspaceTextFile(client, root, path, snapshot)
      queryClient.setQueryData(
        daemonKeys.workspaceFile(config.address, root, path),
        snapshot,
      )
      setBuffers((current) => current[path]
        ? {
            ...current,
            [path]: {
              ...current[path],
              diskContent: snapshot,
              saving: false,
            },
          }
        : current)
      void queryClient.invalidateQueries({
        queryKey: ['daemon', config.address, 'workspace-diff', root],
      })
    } catch (error) {
      setBuffers((current) => current[path]
        ? { ...current, [path]: { ...current[path], saving: false } }
        : current)
      toast.error(t('files.could_not_save', { path, error: errorMessage(error) }))
    }
  }, [client, config, queryClient, root, selected])

  useEffect(() => {
    if (!active) return
    const save = (event: globalThis.KeyboardEvent) => {
      if (event.key.toLowerCase() !== 's' || (!event.metaKey && !event.ctrlKey)) return
      if (event.altKey) return
      event.preventDefault()
      void saveSelected()
    }
    window.addEventListener('keydown', save, true)
    return () => window.removeEventListener('keydown', save, true)
  }, [active, saveSelected])

  const activateTreeEntry = useCallback((entry: WorkingTreeEntry) => {
    if (entry.isDir) {
      setExpanded((current) => current.includes(entry.absolutePath)
        ? current.filter((path) => path !== entry.absolutePath)
        : [...current, entry.absolutePath])
    } else {
      onOpenFile(tabId, entry.relativePath, treeWidthRef.current)
    }
  }, [onOpenFile, tabId])

  const workingTreeEntries = tree.data ?? []
  const selectedTreeEntry = selected
    ? workingTreeEntries.find((entry) => entry.relativePath === selected)
    : undefined
  const treeTabStop = focusedTreeEntry && workingTreeEntries.some((entry) => entry.absolutePath === focusedTreeEntry)
    ? focusedTreeEntry
    : selectedTreeEntry?.absolutePath ?? workingTreeEntries[0]?.absolutePath

  const focusWorkingTreeIndex = useCallback((index: number) => {
    const entry = workingTreeEntries[index]
    if (!entry) return
    setFocusedTreeEntry(entry.absolutePath)
    focusVirtualTreeRow(treeList, index, workingTreeRowId(entry.absolutePath))
  }, [workingTreeEntries])

  const handleWorkingTreeKeyDown = useCallback((
    event: ReactKeyboardEvent<HTMLButtonElement>,
    entry: WorkingTreeEntry,
    index: number,
  ) => {
    if (!isTreeNavigationKey(event.key)) return
    event.preventDefault()
    const action = treeNavigationAction(
      workingTreeEntries.map((candidate) => ({
        depth: candidate.depth,
        directory: candidate.isDir,
        expanded: candidate.isDir && expanded.includes(candidate.absolutePath),
      })),
      index,
      event.key,
    )
    if (action.toggle) {
      setFocusedTreeEntry(entry.absolutePath)
      activateTreeEntry(entry)
    } else {
      focusWorkingTreeIndex(action.index)
    }
  }, [activateTreeEntry, expanded, focusWorkingTreeIndex, workingTreeEntries])

  const updateSelectedBuffer = useCallback((content: string) => {
    if (!selected) return
    setBuffers((current) => {
      const buffer = current[selected]
      const fallback = file.data === undefined
        ? undefined
        : { content: file.data, diskContent: file.data, revision: 0, saving: false }
      const next = buffer ?? fallback
      if (!next || next.content === content) return current
      return { ...current, [selected]: { ...next, content } }
    })
  }, [file.data, selected])

  const retainSelectedEditor = useCallback((editor: Editor<undefined>) => {
    if (!selected) return
    setBuffers((current) => {
      const buffer = current[selected]
      const fallback = file.data === undefined
        ? undefined
        : { content: file.data, diskContent: file.data, revision: 0, saving: false }
      const next = buffer ?? fallback
      if (!next || next.editor === editor) return current
      return { ...current, [selected]: { ...next, editor } }
    })
  }, [file.data, selected, setBuffers])

  const refreshSelectedBuffer = useCallback(() => {
    if (!selected) return
    const buffer = buffersRef.current[selected]
    if (!buffer || buffer.content === buffer.diskContent) void refetchFile()
  }, [refetchFile, selected])

  if (!root) return <PanelMessage title={t('files.no_project_open')} detail={t('files.no_project_open_description')} />

  const fileTree = (
    <div
      className={cn('relative flex min-h-0 flex-col', selected ? 'shrink-0 border-l' : 'flex-1')}
      style={selected ? { width: fittedTreeWidth } : undefined}
    >
      {selected && (
        <PanelResizeHandle
          edge="left"
          label={t('files.resize_tree')}
          max={maxTreeWidth}
          min={140}
          value={fittedTreeWidth}
          onChange={setTreeWidth}
        />
      )}
      <div className="flex h-[42px] shrink-0 items-center gap-2 border-b px-4 text-[11.5px] font-medium text-[var(--text-secondary)]">
        <WakuIcon className="size-[13px] text-[var(--text-tertiary)]" name="folder" />
        <span className="min-w-0 flex-1 truncate">
          {project ? projectDisplayName(project, t('project.no_project_name')) : ''}
        </span>
      </div>
      <div className="relative min-h-0 flex-1">
        {tree.isPending ? (
          <p className="p-3 text-[11px] text-[var(--text-tertiary)]">{t('files.loading')}</p>
        ) : tree.error ? (
          <p className="p-3 text-[11px] text-destructive">{errorMessage(tree.error)}</p>
        ) : (
          <Virtuoso
            aria-label={t('files.workspace_files')}
            className="size-full py-1"
            computeItemKey={(_, entry) => entry.absolutePath}
            data={tree.data ?? []}
            fixedItemHeight={30}
            increaseViewportBy={180}
            itemContent={(index, entry) => (
              <TreeRow
                entry={entry}
                expanded={entry.isDir && expanded.includes(entry.absolutePath)}
                id={workingTreeRowId(entry.absolutePath)}
                selected={selected === entry.relativePath}
                tabIndex={treeTabStop === entry.absolutePath ? 0 : -1}
                onActivate={activateTreeEntry}
                onFocus={() => setFocusedTreeEntry(entry.absolutePath)}
                onKeyDown={(event) => handleWorkingTreeKeyDown(event, entry, index)}
              />
            )}
            ref={treeList}
            role="tree"
          />
        )}
      </div>
    </div>
  )

  if (!selected) return fileTree
  const buffer = buffers[selected] ?? (file.data === undefined
    ? undefined
    : {
        content: file.data,
        diskContent: file.data,
        revision: 0,
        saving: false,
      })
  return (
    <div className="flex min-h-0 flex-1">
      <div className="flex min-h-0 min-w-0 flex-1 flex-col">
        <div className="flex h-[42px] shrink-0 items-center gap-2 border-b px-4 text-[11px] text-[var(--text-secondary)]">
          <FileTypeIcon className="size-[13px]" path={selected} />
          <span className="min-w-0 flex-1 truncate">{selected}</span>
        </div>
        {!buffer && file.isPending
          ? <PanelMessage title={t('files.loading_file')} detail={t('files.reading_from_daemon')} />
          : !buffer && file.error
            ? <PanelMessage title={t('files.file_unavailable')} detail={errorMessage(file.error)} danger />
            : buffer && (
                <Suspense fallback={<PanelMessage title={t('files.loading_editor')} detail={t('files.preparing_syntax')} />}>
                  <CodeFileSurface
                    cacheKey={`editor:${selected}:${buffer.revision}`}
                    contents={buffer.content}
                    editor={buffer.editor}
                    key={`${selected}:${buffer.revision}`}
                    path={selected}
                    onChange={updateSelectedBuffer}
                    onEditor={retainSelectedEditor}
                    onFocus={refreshSelectedBuffer}
                  />
                </Suspense>
              )}
      </div>
      {fileTree}
    </div>
  )
}

interface TreeRowProps {
  entry: WorkingTreeEntry
  expanded: boolean
  id: string
  selected: boolean
  tabIndex: number
  onActivate: (entry: WorkingTreeEntry) => void
  onFocus: () => void
  onKeyDown: (event: ReactKeyboardEvent<HTMLButtonElement>) => void
}

function TreeRow({
  entry,
  expanded,
  id,
  selected,
  tabIndex,
  onActivate,
  onFocus,
  onKeyDown,
}: TreeRowProps) {
  return (
    <button
      aria-expanded={entry.isDir ? expanded : undefined}
      aria-level={entry.depth + 1}
      className={cn(
        'mx-2 flex h-[30px] min-w-0 items-center gap-1.5 rounded-md pr-2 text-left text-[11.5px] outline-none hover:bg-accent focus-visible:bg-accent',
        selected && 'bg-accent',
      )}
      id={id}
      role="treeitem"
      style={{ paddingLeft: `${8 + entry.depth * 16}px`, width: 'calc(100% - 16px)' }}
      tabIndex={tabIndex}
      type="button"
      onClick={() => onActivate(entry)}
      onFocus={onFocus}
      onKeyDown={onKeyDown}
    >
      {entry.isDir
        ? expanded
          ? <WakuIcon className="size-2.5 shrink-0 text-[var(--text-ghost)]" name="chevronDown" />
          : <WakuIcon className="size-2.5 shrink-0 text-[var(--text-ghost)]" name="chevronRight" />
        : <span className="size-2.5 shrink-0" />}
      {entry.isDir
        ? <WakuIcon className="size-3.5 shrink-0 text-[var(--text-tertiary)]" name="folder" />
        : <FileTypeIcon className="size-3.5" path={entry.name} />}
      <span className="truncate">{entry.name}</span>
    </button>
  )
}

function ChangesPanel({
  session,
  project,
  diffSource,
  panelWidth,
  onDiffSourceChange,
}: {
  session: AgentSession | null
  project?: Project
  diffSource: ReviewDiffSource
  panelWidth: number
  onDiffSourceChange: (source: ReviewDiffSource) => void
}) {
  const { t } = useI18n()
  const { client, config, phase } = useDaemon()
  const diffView = useRef<CodeDiffSurfaceHandle>(null)
  const diffTreeList = useRef<VirtuosoHandle>(null)
  const [files, setFiles] = useState<DiffSurfaceFile[]>([])
  const [selectedFile, setSelectedFile] = useState<string | null>(null)
  const [focusedDiffRow, setFocusedDiffRow] = useState<string | null>(null)
  const [expandedPaths, setExpandedPaths] = useState<Set<string>>(new Set())
  const [filter, setFilter] = useState('')
  const [treeWidth, setTreeWidth] = useState(() => readStoredWidth('waku.diffTreeWidth', 184, 140, 360))
  const root = session && project ? sessionCwd(session, project) : undefined
  const maxTreeWidth = Math.max(140, Math.min(360, panelWidth - 140))
  const fittedTreeWidth = clamp(treeWidth, 140, maxTreeWidth)
  const latestTurnSource = latestReviewTurnSource(session)
  const sourceLabel = reviewDiffSourceLabel(diffSource, latestTurnSource, t)
  const diff = useQuery({
    queryKey: daemonKeys.workspaceDiff(config?.address ?? 'disconnected', root ?? 'none', diffSource),
    queryFn: () => collectWorkspaceDiff(requireClient(client), root!, diffSource),
    enabled: phase === 'connected' && Boolean(client && config && root),
  })
  const reviewFiles = mergeReviewDiffFiles(files, diff.data?.numstat ?? '') as DiffSurfaceFile[]

  useEffect(() => {
    setExpandedPaths(diffDirectoryPaths(reviewFiles))
    setSelectedFile((current) => reviewFiles.some((file) => file.id === current)
      ? current
      : reviewFiles[0]?.id ?? null)
    setFilter('')
  }, [files, diff.data?.numstat])

  useEffect(() => {
    setFiles([])
    setSelectedFile(null)
    setExpandedPaths(new Set())
    setFilter('')
  }, [diffSource])

  useEffect(() => {
    const timer = window.setTimeout(() => {
      window.localStorage.setItem('waku.diffTreeWidth', String(Math.round(treeWidth)))
    }, 150)
    return () => window.clearTimeout(timer)
  }, [treeWidth])

  const treeRows = buildDiffTreeRows(reviewFiles, expandedPaths, filter)
  const selectedDiffRow = selectedFile
    ? treeRows.find((row) => row.kind === 'file' && row.file.id === selectedFile)
    : undefined
  const diffTreeTabStop = focusedDiffRow && treeRows.some((row) => diffTreeRowKey(row) === focusedDiffRow)
    ? focusedDiffRow
    : selectedDiffRow ? diffTreeRowKey(selectedDiffRow) : treeRows[0] ? diffTreeRowKey(treeRows[0]) : null

  const toggleDiffDirectory = useCallback((path: string) => {
    setExpandedPaths((current) => {
      const next = new Set(current)
      if (next.has(path)) next.delete(path)
      else next.add(path)
      return next
    })
  }, [])

  const focusDiffTreeIndex = useCallback((index: number) => {
    const row = treeRows[index]
    if (!row) return
    const key = diffTreeRowKey(row)
    setFocusedDiffRow(key)
    focusVirtualTreeRow(diffTreeList, index, diffTreeRowId(key))
  }, [treeRows])

  const handleDiffTreeKeyDown = useCallback((
    event: ReactKeyboardEvent<HTMLButtonElement>,
    row: DiffTreeRow,
    index: number,
  ) => {
    if (!isTreeNavigationKey(event.key)) return
    event.preventDefault()
    const action = treeNavigationAction(
      treeRows.map((candidate) => ({
        depth: candidate.depth,
        directory: candidate.kind === 'directory',
        expanded: candidate.kind === 'directory' && candidate.expanded,
      })),
      index,
      event.key,
    )
    if (action.toggle && row.kind === 'directory') {
      setFocusedDiffRow(diffTreeRowKey(row))
      toggleDiffDirectory(row.path)
    } else {
      focusDiffTreeIndex(action.index)
    }
  }, [focusDiffTreeIndex, toggleDiffDirectory, treeRows])

  if (!root) return <PanelMessage title={t('files.no_project_open')} detail={t('files.no_project_open_description')} />
  const stats = parseNumstat(diff.data?.numstat ?? '')
  const selectSource = (source: ReviewDiffSource) => {
    if (!sameReviewDiffSource(diffSource, source)) onDiffSourceChange(source)
  }
  const sourceItems = [
    {
      id: 'last-turn',
      label: t('diff.source_last_turn'),
      disabled: !latestTurnSource,
      selected: Boolean(latestTurnSource && sameReviewDiffSource(diffSource, latestTurnSource)),
      onSelect: () => latestTurnSource && selectSource(latestTurnSource),
    },
    {
      id: 'uncommitted',
      label: t('diff.source_uncommitted'),
      separatorBefore: true,
      selected: diffSource === 'uncommitted',
      onSelect: () => selectSource('uncommitted'),
    },
    {
      id: 'unstaged',
      label: t('diff.source_unstaged'),
      selected: diffSource === 'unstaged',
      onSelect: () => selectSource('unstaged'),
    },
    {
      id: 'staged',
      label: t('diff.source_staged'),
      selected: diffSource === 'staged',
      onSelect: () => selectSource('staged'),
    },
    {
      id: 'committed',
      label: t('diff.source_committed'),
      separatorBefore: true,
      selected: diffSource === 'committed',
      onSelect: () => selectSource('committed'),
    },
    {
      id: 'branch',
      label: t('diff.source_branch'),
      selected: diffSource === 'branch',
      onSelect: () => selectSource('branch'),
    },
  ]

  let reviewContent: ReactNode
  if (diff.isPending) {
    reviewContent = <PanelMessage title={t('diff.loading')} detail={t('diff.loading_description')} />
  } else if (diff.error) {
    reviewContent = <PanelMessage title={t('diff.unavailable')} detail={errorMessage(diff.error)} danger />
  } else if (!diff.data?.patch.trim()) {
    reviewContent = <PanelMessage title={t('diff.no_changes')} detail={t('diff.no_changes_description')} />
  } else {
    const data = diff.data
    reviewContent = (
      <div className="flex min-h-0 flex-1">
        <div className="flex min-w-0 flex-1">
          <Suspense fallback={<PanelMessage title={t('diff.loading')} detail={t('files.preparing_syntax')} />}>
            <CodeDiffSurface
              completeContext={data.completeContext}
              onFiles={setFiles}
              patch={data.patch}
              ref={diffView}
            />
          </Suspense>
        </div>
        <div
          className="relative flex min-h-0 shrink-0 flex-col border-l bg-background"
          style={{ width: fittedTreeWidth }}
        >
          <PanelResizeHandle
            edge="left"
            label={t('diff.resize_tree')}
            max={maxTreeWidth}
            min={140}
            value={fittedTreeWidth}
            onChange={setTreeWidth}
          />
          <label className="flex h-11 shrink-0 items-center gap-2 border-b px-2">
            <WakuIcon className="size-[13px] shrink-0 text-[var(--text-tertiary)]" name="search" />
            <input
              aria-label={t('diff.filter_files')}
              className="min-w-0 flex-1 bg-transparent text-[11px] outline-none placeholder:text-[var(--text-ghost)]"
              placeholder={t('diff.filter_files')}
              value={filter}
              onChange={(event) => setFilter(event.target.value)}
            />
          </label>
          <Virtuoso
            aria-label={t('diff.changed_files')}
            className="min-h-0 flex-1 py-1"
            computeItemKey={(_, row) => row.kind === 'directory'
              ? `directory:${row.path}`
              : `file:${row.file.id}`}
            data={treeRows}
            fixedItemHeight={30}
            increaseViewportBy={180}
            itemContent={(index, row) => row.kind === 'directory' ? (
              <button
                aria-expanded={row.expanded}
                aria-level={row.depth + 1}
                className="mx-1.5 my-0.5 flex h-[26px] min-w-0 items-center gap-1.5 rounded-md pr-2 text-left text-[11px] text-[var(--text-secondary)] outline-none hover:bg-accent focus-visible:bg-accent"
                id={diffTreeRowId(diffTreeRowKey(row))}
                role="treeitem"
                style={{ paddingLeft: `${7 + row.depth * 14}px`, width: 'calc(100% - 12px)' }}
                tabIndex={diffTreeTabStop === diffTreeRowKey(row) ? 0 : -1}
                type="button"
                onClick={() => toggleDiffDirectory(row.path)}
                onFocus={() => setFocusedDiffRow(diffTreeRowKey(row))}
                onKeyDown={(event) => handleDiffTreeKeyDown(event, row, index)}
              >
                <WakuIcon className="size-2.5 shrink-0 text-[var(--text-ghost)]" name={row.expanded ? 'chevronDown' : 'chevronRight'} />
                <WakuIcon className="size-[13px] shrink-0 text-[var(--text-tertiary)]" name="folder" />
                <span className="min-w-0 flex-1 truncate font-medium">{row.name}</span>
              </button>
            ) : (
              <button
                aria-level={row.depth + 1}
                aria-selected={selectedFile === row.file.id}
                className={cn(
                  'mx-1.5 my-0.5 flex h-[26px] min-w-0 items-center gap-1.5 rounded-md pr-2 text-left text-[11px] text-[var(--text-secondary)] outline-none hover:bg-accent focus-visible:bg-accent',
                  selectedFile === row.file.id && 'bg-accent text-foreground',
                )}
                id={diffTreeRowId(diffTreeRowKey(row))}
                role="treeitem"
                style={{ paddingLeft: `${23 + row.depth * 14}px`, width: 'calc(100% - 12px)' }}
                tabIndex={diffTreeTabStop === diffTreeRowKey(row) ? 0 : -1}
                title={row.file.path}
                type="button"
                onClick={() => {
                  setSelectedFile(row.file.id)
                  diffView.current?.scrollToFile(row.file.id)
                }}
                onFocus={() => setFocusedDiffRow(diffTreeRowKey(row))}
                onKeyDown={(event) => handleDiffTreeKeyDown(event, row, index)}
              >
                <FileTypeIcon className="size-[13px]" path={row.file.path} />
                <span className="min-w-0 flex-1 truncate">{fileName(row.file.path)}</span>
                <DiffFileStatus status={row.file.status} />
              </button>
            )}
            ref={diffTreeList}
            role="tree"
          />
        </div>
      </div>
    )
  }

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div className="flex min-h-10 items-center gap-2 border-b px-3 text-[11px]">
        <ControlMenu
          items={sourceItems}
          label={sourceLabel}
          menuClassName="w-44"
          placement="below"
          triggerClassName="h-7 max-w-44 bg-background px-2"
        />
        {diff.data && (
          <>
            <span className="font-medium text-[var(--success)]">+{stats.additions}</span>
            <span className="font-medium text-destructive">-{stats.deletions}</span>
            {!diff.data.completeContext && (
              <span className="truncate text-[var(--text-tertiary)]">{t('diff.truncated')}</span>
            )}
          </>
        )}
        <div className="flex-1" />
        <button aria-label={t('diff.refresh')} className="rounded p-1 hover:bg-accent" type="button" onClick={() => void diff.refetch()}>
          <WakuIcon className={cn('size-3.5', diff.isFetching && 'motion-safe:animate-spin')} name="rotateCw" />
        </button>
      </div>
      {reviewContent}
    </div>
  )
}

type DiffTreeRow = {
  kind: 'directory'
  path: string
  name: string
  depth: number
  expanded: boolean
} | {
  kind: 'file'
  file: DiffSurfaceFile
  depth: number
}

function buildDiffTreeRows(
  files: DiffSurfaceFile[],
  expandedPaths: Set<string>,
  filter: string,
): DiffTreeRow[] {
  const needle = filter.trim().toLocaleLowerCase()
  const filtering = needle.length > 0
  const rows: DiffTreeRow[] = []
  const emittedDirectories = new Set<string>()

  for (const file of [...files]
    .filter((candidate) => !filtering || candidate.path.toLocaleLowerCase().includes(needle))
    .sort((left, right) => left.path.localeCompare(right.path))) {
    const parts = file.path.split('/').filter(Boolean)
    let directory = ''
    let visible = true
    for (const [depth, part] of parts.slice(0, -1).entries()) {
      directory = directory ? `${directory}/${part}` : part
      const expanded = filtering || expandedPaths.has(directory)
      if (!emittedDirectories.has(directory) && visible) {
        emittedDirectories.add(directory)
        rows.push({ kind: 'directory', path: directory, name: part, depth, expanded })
      }
      if (!expanded) {
        visible = false
        break
      }
    }
    if (visible) rows.push({ kind: 'file', file, depth: Math.max(0, parts.length - 1) })
  }
  return rows
}

function diffDirectoryPaths(files: DiffSurfaceFile[]): Set<string> {
  const paths = new Set<string>()
  for (const file of files) {
    const parts = file.path.split('/').filter(Boolean)
    let directory = ''
    for (const part of parts.slice(0, -1)) {
      directory = directory ? `${directory}/${part}` : part
      paths.add(directory)
    }
  }
  return paths
}

function DiffFileStatus({ status }: { status: DiffSurfaceFile['status'] }) {
  return (
    <span className={cn(
      'grid size-4 shrink-0 place-items-center rounded border text-[9px] font-semibold',
      status === 'A' && 'border-[var(--success)]/60 text-[var(--success)]',
      status === 'D' && 'border-destructive/60 text-destructive',
      (status === 'B' || status === 'M') && 'border-[var(--warning)]/60 text-[var(--warning)]',
    )}>
      {status}
    </span>
  )
}

function fileName(path: string): string {
  return path.split('/').at(-1) ?? path
}

function BackgroundWorkPanel({
  session,
  workKey,
  onTitle,
}: {
  session: AgentSession | null
  workKey: BackgroundWorkKey
  onTitle: (title: string) => void
}) {
  const { t } = useI18n()
  const { backgroundWork, stopBackgroundWork } = useRuntime()
  const reportedTitle = useRef<string | null>(null)
  const item = session
    ? backgroundWork[session.id]?.find((candidate) => sameBackgroundWorkKey(candidate.key, workKey))
    : undefined

  useEffect(() => {
    if (!item?.title) return
    const reportKey = `${item.key.kind}:${item.key.providerId}:${item.title}`
    if (reportedTitle.current === reportKey) return
    reportedTitle.current = reportKey
    onTitle(item.title)
  }, [item?.title, onTitle])

  if (!session || !item) {
    return (
      <div className="grid min-h-0 flex-1 place-items-center p-6 text-center">
        <div>
          <WakuIcon className="mx-auto size-[22px] text-[var(--text-ghost)]" name={backgroundWorkKindIcon(workKey.kind)} />
          <p className="mt-2 text-[12px] text-[var(--text-secondary)]">{t('background.unavailable')}</p>
        </div>
      </div>
    )
  }

  const stoppable = isStoppableBackgroundStatus(item.status) && item.canStop && item.controlId
  const metadata = [
    [t(item.key.kind === 'subagent' ? 'background.prompt' : 'background.command'), item.command],
    [t('background.cwd'), item.cwd],
    [t('background.role'), item.role],
    [t('background.model'), item.model],
    [t('background.latest_update'), item.detail],
    [t('background.exit_code'), item.exitCode == null ? null : String(item.exitCode)],
  ].filter((entry): entry is [string, string] => Boolean(entry[1]))
  return (
    <div className="min-h-0 flex-1 overflow-auto p-3">
      <div className="overflow-hidden rounded-[9px] border bg-card">
        <div className="flex min-h-[54px] items-center gap-2.5 px-[11px] py-2">
          <WakuIcon className="size-[15px] text-[var(--text-secondary)]" name={backgroundWorkKindIcon(item.key.kind)} />
          <div className="min-w-0 flex-1">
            <div className="truncate text-[12px] font-medium">{item.title}</div>
            <div className="mt-1 flex items-center gap-1.5 text-[10px] text-[var(--text-tertiary)]">
              <BackgroundStatusIcon status={item.status} />
              <span>{backgroundStatusLabel(item.status, t)}</span>
              <span>·</span>
              <span>{backgroundElapsed(item, t)}</span>
            </div>
          </div>
          {stoppable && (
            <Button
              className="h-[26px] gap-1.5 px-[9px] text-[10.5px] hover:bg-destructive/10"
              size="sm"
              variant="outline"
              onClick={() => void stopBackgroundWork(session.id, item).catch(() => {})}
            >
              <WakuIcon className="size-[11px] text-destructive" name="stopFilled" />
              {t('background.stop')}
            </Button>
          )}
        </div>
        {metadata.map(([label, value]) => (
          <div className="border-t px-2.5 py-[7px]" key={label}>
            <div className="text-[9.5px] text-[var(--text-tertiary)]">{label}</div>
            <div className="mt-[3px] whitespace-pre-wrap break-words font-mono text-[10.5px] text-[var(--text-secondary)]">{value}</div>
          </div>
        ))}
        <div className="border-t p-2.5">
          <div className="mb-[5px] flex items-center justify-between text-[9.5px] text-[var(--text-tertiary)]">
            <span>{t('background.output')}</span>
            {item.outputTruncated && <span>{t('background.output_truncated')}</span>}
          </div>
          <pre className="max-h-80 overflow-auto rounded-md bg-[var(--inset)] p-2 font-mono text-[10.5px] leading-[15px] text-[var(--text-secondary)]">
            {stripAnsi(item.output || t('background.no_output'))}
          </pre>
        </div>
      </div>
    </div>
  )
}

function BackgroundStatusIcon({ status }: { status: BackgroundWorkStatus }) {
  const icon: WakuIconName = status === 'completed'
    ? 'check'
    : status === 'failed'
      ? 'x'
      : status === 'lost'
        ? 'alert'
        : status === 'stopping' || status === 'stopped'
          ? 'stop'
          : 'loaderCircle'
  return (
    <WakuIcon
      className={cn(
        'size-[9px]',
        isStoppableBackgroundStatus(status) && 'motion-safe:animate-spin text-ring',
        status === 'completed' && 'text-[var(--success)]',
        (status === 'failed' || status === 'lost') && 'text-destructive',
      )}
      name={icon}
    />
  )
}

function TerminalPanel({
  terminalId,
  session,
  project,
  onTitle,
}: {
  terminalId: string
  session: AgentSession | null
  project?: Project
  onTitle: (title: string) => void
}) {
  const { t } = useI18n()
  const { client, phase } = useDaemon()
  const { ref, write, focus } = useTerminal()
  const [core, setCore] = useState<GhosttyCore | null>(null)
  const ready = useRef(false)
  const queuedOutput = useRef<Uint8Array[]>([])
  const titleScanner = useRef(new TerminalTitleScanner())
  const [error, setError] = useState<string | null>(null)
  const [exited, setExited] = useState(false)
  const cwd = session && project ? sessionCwd(session, project) : undefined

  useEffect(() => {
    let disposed = false
    void GhosttyCore.load()
      .then((loaded) => {
        if (!disposed) setCore(loaded)
      })
      .catch((cause) => {
        if (!disposed) setError(errorMessage(cause))
      })
    return () => {
      disposed = true
    }
  }, [])

  useEffect(() => {
    if (!client || phase !== 'connected' || !cwd) return
    let disposed = false
    titleScanner.current = new TerminalTitleScanner()
    const unsubscribe = client.subscribe(terminalId, terminalId, (event) => {
      if (event.event.kind === 'terminalOutput') {
        const payload = event.event.payload as { data?: string }
        if (!payload.data) return
        const bytes = decodeBase64(payload.data)
        const title = titleScanner.current.feed(bytes)
        if (title) onTitle(title)
        if (ready.current) write(bytes)
        else queuedOutput.current.push(bytes)
      } else if (event.event.kind === 'terminalExited') {
        setExited(true)
      } else if (event.event.kind === 'terminalError') {
        setError(typeof event.event.payload === 'string' ? event.event.payload : t('terminal.transport_failed'))
      }
    })

    void client.request(
      { type: 'openTerminal', cwd, cols: 80, rows: 24 },
      terminalId,
      terminalId,
    ).catch((cause) => {
      if (!disposed) setError(errorMessage(cause))
    })

    return () => {
      disposed = true
      ready.current = false
      queuedOutput.current = []
      unsubscribe()
      void client.notify({ type: 'closeTerminal' }, terminalId, terminalId).catch(() => {})
    }
  }, [client, cwd, phase, terminalId, write])

  if (!cwd) return <PanelMessage title={t('files.no_project_open')} detail={t('terminal.no_workspace_description')} />
  return (
    <div className="relative flex min-h-0 flex-1 bg-background">
      {core && (
        <Terminal
          autoResize
          className="waku-terminal size-full"
          core={core}
          cursorBlink
          ref={ref}
          onData={(data) => {
            if (!client || phase !== 'connected' || exited) return
            void client.notify(
              { type: 'writeTerminal', data: encodeBase64(new TextEncoder().encode(data)) },
              terminalId,
              terminalId,
            ).catch((cause) => setError(errorMessage(cause)))
          }}
          onError={(cause) => setError(errorMessage(cause))}
          onReady={() => {
            ready.current = true
            for (const bytes of queuedOutput.current) write(bytes)
            queuedOutput.current = []
            focus()
          }}
          onResize={(cols, rows) => {
            if (!client || phase !== 'connected') return
            void client.notify(
              { type: 'resizeTerminal', cols: clampU16(cols), rows: clampU16(rows) },
              terminalId,
              terminalId,
            ).catch(() => {})
          }}
        />
      )}
      {(error || exited) && (
        <div className="pointer-events-none absolute bottom-2 right-2 max-w-[calc(100%-16px)] rounded-md border bg-popover px-2 py-1 text-[10.5px] text-[var(--text-secondary)] shadow-sm">
          {error ?? t('terminal.process_exited')}
        </div>
      )}
    </div>
  )
}

class TerminalTitleScanner {
  private readonly decoder = new TextDecoder()
  private pending = ''

  feed(bytes: Uint8Array): string | null {
    const text = this.pending + this.decoder.decode(bytes, { stream: true })
    const pattern = /\x1b\](?:0|2);([^\x07\x1b]*)(?:\x07|\x1b\\)/g
    let title: string | null = null
    let consumed = 0
    for (const match of text.matchAll(pattern)) {
      title = match[1]?.replace(/[\x00-\x1f\x7f]/g, '').trim().slice(0, 80) || null
      consumed = (match.index ?? 0) + match[0].length
    }

    const incompleteOsc = text.lastIndexOf('\x1b]')
    if (incompleteOsc >= consumed) this.pending = text.slice(incompleteOsc, incompleteOsc + 1_024)
    else this.pending = text.endsWith('\x1b') ? '\x1b' : ''
    return title
  }
}

function PanelMessage({ title, detail, danger = false }: { title: string; detail: string; danger?: boolean }) {
  return (
    <div className="grid min-h-0 flex-1 place-items-center p-6 text-center">
      <div className="max-w-64">
        <div className={cn('text-[13px] font-medium', danger && 'text-destructive')}>{title}</div>
        <p className="mt-1.5 text-[11px] leading-4 text-[var(--text-tertiary)]">{detail}</p>
      </div>
    </div>
  )
}

function parseNumstat(numstat: string) {
  let files = 0
  let additions = 0
  let deletions = 0
  for (const line of numstat.trim().split('\n')) {
    if (!line) continue
    const [added, removed] = line.split('\t')
    files += 1
    additions += Number.parseInt(added || '0', 10) || 0
    deletions += Number.parseInt(removed || '0', 10) || 0
  }
  return { files, additions, deletions }
}

function encodeBase64(bytes: Uint8Array): string {
  let binary = ''
  for (let offset = 0; offset < bytes.length; offset += 0x8000) {
    binary += String.fromCharCode(...bytes.subarray(offset, offset + 0x8000))
  }
  return btoa(binary)
}

function decodeBase64(value: string): Uint8Array {
  const binary = atob(value)
  const bytes = new Uint8Array(binary.length)
  for (let index = 0; index < binary.length; index += 1) bytes[index] = binary.charCodeAt(index)
  return bytes
}

function clampU16(value: number): number {
  return Math.max(1, Math.min(65_535, Math.round(value)))
}

function clamp(value: number, min: number, max: number): number {
  return Math.min(max, Math.max(min, value))
}

function panelTabId(tabId: string): string {
  return `right-panel-tab-${tabId}`
}

function panelContentId(tabId: string): string {
  return `right-panel-content-${tabId}`
}

function isTreeNavigationKey(key: string): key is TreeNavigationKey {
  return ['ArrowLeft', 'ArrowRight', 'ArrowUp', 'ArrowDown', 'Home', 'End'].includes(key)
}

function workingTreeRowId(path: string): string {
  return `working-tree-row-${encodeURIComponent(path)}`
}

function diffTreeRowKey(row: DiffTreeRow): string {
  return row.kind === 'directory' ? `directory:${row.path}` : `file:${row.file.id}`
}

function diffTreeRowId(key: string): string {
  return `diff-tree-row-${encodeURIComponent(key)}`
}

function focusVirtualTreeRow(
  list: { current: VirtuosoHandle | null },
  index: number,
  id: string,
) {
  list.current?.scrollIntoView({ index, behavior: 'auto' })
  let attempts = 0
  const focus = () => {
    const row = document.getElementById(id)
    if (row) {
      row.focus()
      return
    }
    attempts += 1
    if (attempts < 4) window.requestAnimationFrame(focus)
  }
  window.requestAnimationFrame(focus)
}

function readStoredWidth(key: string, fallback: number, min: number, max: number): number {
  if (typeof window === 'undefined') return fallback
  const raw = window.localStorage.getItem(key)
  if (raw === null) return fallback
  const value = Number(raw)
  return Number.isFinite(value) ? clamp(value, min, max) : fallback
}

function absoluteParentPaths(root: string | undefined, relativePath: string) {
  if (!root) return []
  const separator = root.includes('\\') ? '\\' : '/'
  const normalizedRoot = root.replace(/[\\/]+$/, '') || separator
  const parts = relativePath.split(/[\\/]/).filter(Boolean)
  const paths: string[] = []
  let current = normalizedRoot
  for (const part of parts.slice(0, -1)) {
    current = current === separator ? `${current}${part}` : `${current}${separator}${part}`
    paths.push(current)
  }
  return paths
}

function useViewportWidth(): number {
  const [width, setWidth] = useState(() => typeof window === 'undefined' ? 1_440 : window.innerWidth)

  useEffect(() => {
    const update = () => setWidth(window.innerWidth)
    window.addEventListener('resize', update)
    return () => window.removeEventListener('resize', update)
  }, [])

  return width
}

function sameBackgroundWorkKey(left: BackgroundWorkKey, right: BackgroundWorkKey) {
  return left.kind === right.kind && left.providerId === right.providerId
}

function backgroundWorkKindIcon(kind?: BackgroundWorkKey['kind']): WakuIconName {
  return kind === 'subagent' ? 'bot' : 'terminalSquare'
}

function backgroundWorkKindLabel(
  kind: BackgroundWorkKey['kind'] | undefined,
  t: Translator,
) {
  return t(kind === 'subagent'
    ? 'background.subagent'
    : kind === 'monitor'
      ? 'background.monitor'
      : 'background.process')
}

function isStoppableBackgroundStatus(status: BackgroundWorkStatus) {
  return status === 'starting' || status === 'running' || status === 'monitoring'
}

function backgroundStatusLabel(status: BackgroundWorkStatus, t: Translator) {
  return t(`background.status.${status}`)
}

function backgroundElapsed(item: BackgroundWorkItem, t: Translator) {
  const duration = item.durationMs ?? Math.max(0, Date.now() - item.startedAtMs)
  return formatWorkingElapsed(Math.floor(duration / 1_000), t)
}

function stripAnsi(value: string) {
  return value.replace(new RegExp('\\u001b\\[[0-?]*[ -/]*[@-~]', 'g'), '')
}

function requireClient<T>(client: T | null): T {
  if (!client) throw new Error('Waku daemon is disconnected')
  return client
}

function errorMessage(error: unknown) {
  return error instanceof Error ? error.message : String(error)
}

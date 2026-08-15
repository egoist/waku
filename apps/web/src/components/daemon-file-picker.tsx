import { useQuery } from '@tanstack/react-query'
import type { WorkingTreeEntry } from '@waku/client'
import { useEffect, useRef, useState, type KeyboardEvent, type RefObject } from 'react'
import { Virtuoso, type VirtuosoHandle } from 'react-virtuoso'
import { Button } from '@/components/ui/button'
import { WakuIcon } from '@/components/waku-icon'
import { browseDaemonDirectory, daemonKeys } from '@/lib/daemon-api'
import { useDaemon } from '@/lib/daemon-context'
import { useI18n } from '@/lib/i18n'
import type { Translator } from '@/lib/transcript-presentation'
import { cn } from '@/lib/utils'

export function DaemonFilePicker({
  root,
  workspaceLabel,
  selectionMode = 'attachment',
  returnFocus,
  onClose,
  onSelect,
}: {
  root: string
  workspaceLabel: string
  selectionMode?: 'attachment' | 'file' | 'directory'
  returnFocus?: RefObject<HTMLElement | null>
  onClose: () => void
  onSelect: (absolutePath: string) => Promise<boolean>
}) {
  const { t } = useI18n()
  const { client, config, phase } = useDaemon()
  const dialog = useRef<HTMLDivElement>(null)
  const previousFocus = useRef<HTMLElement | null>(null)
  const list = useRef<VirtuosoHandle>(null)
  const [history, setHistory] = useState(() => [root])
  const [historyIndex, setHistoryIndex] = useState(0)
  const [filter, setFilter] = useState('')
  const [selectedPath, setSelectedPath] = useState<string | null>(
    selectionMode === 'directory' ? root : null,
  )
  const [submittingPath, setSubmittingPath] = useState<string | null>(null)
  const currentPath = history[historyIndex]!

  useEffect(() => {
    previousFocus.current = document.activeElement instanceof HTMLElement
      ? document.activeElement
      : null
    dialog.current?.focus()
    return () => {
      const previous = returnFocus?.current ?? previousFocus.current
      previousFocus.current = null
      requestAnimationFrame(() => {
        if (previous?.isConnected) previous.focus()
      })
    }
  }, [returnFocus])

  const directory = useQuery({
    queryKey: daemonKeys.directory(config?.address ?? 'disconnected', currentPath),
    queryFn: () => {
      if (!client) throw new Error(t('file_picker.daemon_disconnected'))
      return browseDaemonDirectory(client, currentPath)
    },
    enabled: phase === 'connected' && Boolean(client && config),
  })
  const entries = directory.data?.entries ?? []
  const query = filter.trim().toLocaleLowerCase()
  const visibleEntries = query
    ? entries.filter((entry) => entry.name.toLocaleLowerCase().includes(query))
    : entries
  const selectedEntry = visibleEntries.find((entry) => entry.absolutePath === selectedPath)
  const resolvedPath = directory.data?.path ?? currentPath
  const selectedTarget = selectionMode === 'directory'
    ? selectedEntry?.isDir
      ? selectedEntry.absolutePath
      : selectedPath && samePath(selectedPath, resolvedPath)
        ? resolvedPath
        : null
    : selectedEntry && (selectionMode === 'attachment' || !selectedEntry.isDir)
        ? selectedEntry.absolutePath
        : null

  useEffect(() => {
    setSelectedPath(selectionMode === 'directory' ? currentPath : null)
    setFilter('')
  }, [currentPath, selectionMode])

  useEffect(() => {
    if (
      selectedPath
      && !(selectionMode === 'directory' && samePath(selectedPath, resolvedPath))
      && !visibleEntries.some((entry) => entry.absolutePath === selectedPath)
    ) {
      setSelectedPath(null)
    }
  }, [resolvedPath, selectedPath, selectionMode, visibleEntries])

  function visit(path: string | null | undefined) {
    if (!path || submittingPath) return
    if (samePath(path, currentPath)) {
      if (selectionMode === 'directory') setSelectedPath(currentPath)
      return
    }
    setHistory((current) => [...current.slice(0, historyIndex + 1), path])
    setHistoryIndex(historyIndex + 1)
  }

  function openFolder(entry: WorkingTreeEntry) {
    if (entry.isDir) visit(entry.absolutePath)
  }

  async function select(target = selectedTarget) {
    if (!target || submittingPath) return
    setSubmittingPath(target)
    try {
      if (await onSelect(target)) onClose()
    } finally {
      setSubmittingPath(null)
    }
  }

  function activate(entry = selectedEntry) {
    if (!entry) return
    if (entry.isDir) openFolder(entry)
    else if (selectionMode !== 'directory') void select(entry.absolutePath)
  }

  function moveSelection(delta: number) {
    const selectableEntries = selectionMode === 'directory'
      ? visibleEntries.filter((entry) => entry.isDir)
      : visibleEntries
    if (!selectableEntries.length) return
    const current = selectableEntries.findIndex((entry) => entry.absolutePath === selectedPath)
    const next = current === -1
      ? delta < 0 ? selectableEntries.length - 1 : 0
      : Math.max(0, Math.min(selectableEntries.length - 1, current + delta))
    const entry = selectableEntries[next]!
    setSelectedPath(entry.absolutePath)
    list.current?.scrollIntoView({
      index: visibleEntries.findIndex((item) => item.absolutePath === entry.absolutePath),
      behavior: 'auto',
    })
  }

  function handleKeyDown(event: KeyboardEvent<HTMLDivElement>) {
    const editingFilter = (event.target as HTMLElement).tagName === 'INPUT'
    if (event.key === 'Escape') {
      event.preventDefault()
      onClose()
    } else if (event.key === 'ArrowDown') {
      event.preventDefault()
      moveSelection(1)
    } else if (event.key === 'ArrowUp' && !event.metaKey) {
      event.preventDefault()
      moveSelection(-1)
    } else if (event.key === 'Enter') {
      event.preventDefault()
      if (selectionMode === 'directory') void select()
      else activate()
    } else if ((event.metaKey && event.key === 'ArrowUp') || (!editingFilter && event.key === 'Backspace')) {
      event.preventDefault()
      visit(directory.data?.parent)
    } else if (event.altKey && event.key === 'ArrowLeft' && historyIndex > 0) {
      event.preventDefault()
      setHistoryIndex((current) => current - 1)
    } else if (event.altKey && event.key === 'ArrowRight' && historyIndex < history.length - 1) {
      event.preventDefault()
      setHistoryIndex((current) => current + 1)
    }
  }

  const dialogTitle = t(selectionMode === 'directory' ? 'file_picker.open_project' : 'file_picker.attach')
  const explorerLabel = t(selectionMode === 'directory' ? 'file_picker.daemon_folders' : 'file_picker.daemon_items')

  return (
    <div
      aria-label={t(selectionMode === 'directory'
        ? 'file_picker.choose_project_folder'
        : 'file_picker.attach_from_daemon')}
      aria-modal="true"
      className="fixed inset-0 z-[110] flex items-center justify-center bg-black/20 p-7 backdrop-blur-[1px] dark:bg-black/38"
      ref={dialog}
      role="dialog"
      tabIndex={-1}
      onKeyDown={handleKeyDown}
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) onClose()
      }}
    >
      <div className="flex h-[min(600px,calc(100dvh-56px))] w-full max-w-[820px] flex-col overflow-hidden rounded-xl border bg-card shadow-[0_24px_80px_rgba(0,0,0,0.28)]">
        <header className="relative flex h-11 shrink-0 items-center justify-center border-b bg-[var(--raised)]/70 px-4">
          <h2 className="text-[13px] font-semibold">{dialogTitle}</h2>
          <span className="absolute right-4 text-[10.5px] text-[var(--text-tertiary)]">{explorerLabel}</span>
        </header>

        <div className="flex min-h-0 flex-1">
          <aside className="w-[174px] shrink-0 border-r bg-[var(--raised)]/45 px-2 py-3">
            <div className="px-2 pb-1.5 text-[10px] font-semibold text-[var(--text-tertiary)]">{t('file_picker.locations')}</div>
            <LocationButton
              active={samePath(resolvedPath, root)}
              label={workspaceLabel}
              title={root}
              onClick={() => visit(root)}
            />
            {directory.data?.home && (
              <LocationButton
                active={samePath(resolvedPath, directory.data.home)}
                label={t('file_picker.home')}
                title={directory.data.home}
                onClick={() => visit(directory.data?.home)}
              />
            )}
            {directory.data?.filesystem_root && (
              <LocationButton
                active={samePath(resolvedPath, directory.data.filesystem_root)}
                label={t('file_picker.file_system')}
                title={directory.data.filesystem_root}
                onClick={() => visit(directory.data?.filesystem_root)}
              />
            )}
          </aside>

          <main className="flex min-w-0 flex-1 flex-col bg-background/35">
            <div className="flex h-11 shrink-0 items-center gap-2 border-b px-2.5">
              <div className="flex items-center rounded-md border bg-card shadow-sm">
                <ToolbarButton
                  label={t('file_picker.back')}
                  disabled={historyIndex === 0 || Boolean(submittingPath)}
                  icon="arrowLeft"
                  onClick={() => setHistoryIndex((current) => current - 1)}
                />
                <span className="h-5 w-px bg-border" />
                <ToolbarButton
                  label={t('file_picker.forward')}
                  disabled={historyIndex === history.length - 1 || Boolean(submittingPath)}
                  icon="arrowRight"
                  onClick={() => setHistoryIndex((current) => current + 1)}
                />
              </div>
              <ToolbarButton
                label={t('file_picker.enclosing_folder')}
                disabled={!directory.data?.parent || Boolean(submittingPath)}
                icon="arrowUp"
                onClick={() => visit(directory.data?.parent)}
              />

              <div className="flex min-w-0 flex-1 items-center gap-2 rounded-md px-1.5 py-1" title={resolvedPath}>
                <WakuIcon className="size-[15px] text-[#4c9dea]" name="folder" />
                <span className="truncate text-[11.5px] font-medium">{fileName(resolvedPath) || resolvedPath}</span>
                <span className="min-w-0 truncate text-[10.5px] text-[var(--text-tertiary)]">{resolvedPath}</span>
              </div>

              <label className="flex h-7 w-40 shrink-0 items-center gap-1.5 rounded-md border bg-card px-2 shadow-sm focus-within:ring-1 focus-within:ring-ring">
                <WakuIcon className="size-3 text-[var(--text-tertiary)]" name="search" />
                <input
                  aria-label={t('file_picker.filter_folder')}
                  className="min-w-0 flex-1 bg-transparent text-[11.5px] outline-none placeholder:text-[var(--text-ghost)]"
                  placeholder={t('file_picker.filter')}
                  type="search"
                  value={filter}
                  onChange={(event) => setFilter(event.target.value)}
                />
              </label>
              <ToolbarButton
                label={t('file_picker.refresh')}
                disabled={Boolean(submittingPath)}
                icon="rotateCw"
                spinning={directory.isFetching}
                onClick={() => void directory.refetch()}
              />
            </div>

            <div className="grid h-7 shrink-0 grid-cols-[minmax(0,1fr)_120px] items-center border-b bg-card/65 px-3 text-[10.5px] font-medium text-[var(--text-tertiary)]">
              <span>{t('file_picker.name')}</span>
              <span>{t('file_picker.kind')}</span>
            </div>
            <div className="relative min-h-0 flex-1 bg-card/35">
              {directory.isPending ? (
                <ExplorerMessage icon="folder" title={t('file_picker.loading_folder')} />
              ) : directory.error ? (
                <ExplorerMessage danger icon="alert" title={errorMessage(directory.error)} />
              ) : !visibleEntries.length ? (
                <ExplorerMessage icon="folder" title={t(filter
                  ? 'file_picker.no_matching_items'
                  : 'file_picker.empty_folder')} />
              ) : (
                <Virtuoso
                  aria-label={t(selectionMode === 'directory' ? 'file_picker.folders' : 'file_picker.files')}
                  className="size-full py-1 outline-none"
                  computeItemKey={(_, entry) => entry.absolutePath}
                  data={visibleEntries}
                  fixedItemHeight={30}
                  increaseViewportBy={180}
                  itemContent={(index, entry) => {
                  const selectable = selectionMode !== 'directory' || entry.isDir
                  const selected = selectable && selectedPath === entry.absolutePath
                  return (
                    <button
                      aria-disabled={!selectable}
                      aria-selected={selected}
                      className={cn(
                        'grid h-[30px] w-full grid-cols-[minmax(0,1fr)_120px] items-center px-3 text-left text-[11.5px] outline-none',
                        selected
                          ? 'bg-[#0a84ff] text-white'
                          : index % 2 === 1
                            ? 'bg-foreground/[0.018] hover:bg-accent focus-visible:bg-accent'
                            : 'hover:bg-accent focus-visible:bg-accent',
                        !selectable && 'text-[var(--text-ghost)] hover:bg-transparent',
                      )}
                      role="option"
                      type="button"
                      onClick={() => {
                        if (selectable) setSelectedPath(entry.absolutePath)
                      }}
                      onDoubleClick={() => activate(entry)}
                    >
                      <span className="flex min-w-0 items-center gap-2">
                        <WakuIcon
                          className={cn(
                            'size-[15px]',
                            selected
                              ? 'text-white'
                              : entry.isDir
                                ? 'text-[#4c9dea]'
                                : selectable
                                  ? 'text-[var(--text-tertiary)]'
                                  : 'text-[var(--text-ghost)]',
                          )}
                          name={entry.isDir ? 'folder' : 'file'}
                        />
                        <span className="truncate">{entry.name}</span>
                      </span>
                      <span className={cn('truncate', selected ? 'text-white/80' : 'text-[var(--text-tertiary)]')}>
                        {entryKind(entry, t)}
                      </span>
                    </button>
                  )
                  }}
                  ref={list}
                  role="listbox"
                />
              )}
            </div>
          </main>
        </div>

        <footer className="flex h-[58px] shrink-0 items-center gap-3 border-t bg-[var(--raised)]/55 px-4">
          <div className="min-w-0 flex-1">
            <div className="text-[10px] text-[var(--text-tertiary)]">
              {t(selectionMode === 'directory' ? 'file_picker.selected_folder' : 'file_picker.selected_item')}
            </div>
            <div className="mt-0.5 truncate text-[11.5px]" title={selectedTarget ?? undefined}>
              {selectedTarget ?? t('file_picker.none')}
            </div>
          </div>
          <Button disabled={Boolean(submittingPath)} size="sm" type="button" variant="outline" onClick={onClose}>
            {t('common.cancel')}
          </Button>
          <Button
            disabled={!selectedTarget || Boolean(submittingPath)}
            size="sm"
            type="button"
            onClick={() => void select()}
          >
            {submittingPath
              ? t(selectionMode === 'directory' ? 'file_picker.opening' : 'file_picker.attaching')
              : t(selectionMode === 'directory' ? 'file_picker.open' : 'file_picker.attach')}
          </Button>
        </footer>
      </div>
    </div>
  )
}

function LocationButton({
  active,
  label,
  title,
  onClick,
}: {
  active: boolean
  label: string
  title: string
  onClick: () => void
}) {
  return (
    <button
      className={cn(
        'flex h-8 w-full min-w-0 items-center gap-2 rounded-md px-2 text-left text-[12px] outline-none hover:bg-accent focus-visible:ring-1 focus-visible:ring-ring',
        active && 'bg-accent',
      )}
      title={title}
      type="button"
      onClick={onClick}
    >
      <WakuIcon className="size-[15px] text-[#4c9dea]" name="folder" />
      <span className="truncate">{label}</span>
    </button>
  )
}

function ToolbarButton({
  disabled,
  icon,
  label,
  spinning = false,
  onClick,
}: {
  disabled?: boolean
  icon: 'arrowLeft' | 'arrowRight' | 'arrowUp' | 'rotateCw'
  label: string
  spinning?: boolean
  onClick: () => void
}) {
  return (
    <button
      aria-label={label}
      className="grid size-7 shrink-0 place-items-center rounded-md text-[var(--text-secondary)] outline-none hover:bg-accent focus-visible:ring-1 focus-visible:ring-ring disabled:opacity-30"
      disabled={disabled}
      title={label}
      type="button"
      onClick={onClick}
    >
      <WakuIcon className={cn('size-3.5', spinning && 'motion-safe:animate-spin')} name={icon} />
    </button>
  )
}

function ExplorerMessage({
  danger = false,
  icon,
  title,
}: {
  danger?: boolean
  icon: 'alert' | 'folder'
  title: string
}) {
  return (
    <div className="grid h-full min-h-48 place-items-center px-8 text-center">
      <div>
        <WakuIcon className={cn('mx-auto size-8 text-[var(--text-ghost)]', danger && 'text-destructive')} name={icon} />
        <p className={cn('mt-3 text-[11.5px] text-[var(--text-tertiary)]', danger && 'text-destructive')}>
          {title}
        </p>
      </div>
    </div>
  )
}

function entryKind(entry: WorkingTreeEntry, t: Translator): string {
  if (entry.isDir) return t('file_picker.folder')
  const extension = entry.name.includes('.') ? entry.name.split('.').at(-1) : undefined
  return extension
    ? t('file_picker.typed_file', { type: extension.toLocaleUpperCase() })
    : t('file_picker.file')
}

function fileName(path: string): string {
  return path.split(/[\\/]/).filter(Boolean).at(-1) ?? ''
}

function samePath(left: string, right: string): boolean {
  return left.replace(/[\\/]+$/, '') === right.replace(/[\\/]+$/, '')
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error)
}

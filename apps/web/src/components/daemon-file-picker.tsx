import { useQuery } from '@tanstack/react-query'
import type { WorkingTreeEntry } from '@waku/client'
import { useEffect, useRef, useState, type KeyboardEvent, type RefObject } from 'react'
import { Virtuoso, type VirtuosoHandle } from 'react-virtuoso'
import { Button } from '@/components/ui/button'
import { Dialog, DialogContent, DialogTitle } from '@/components/ui/dialog'
import { FileTypeIcon, WakuIcon } from '@/components/waku-icon'
import { browseDaemonDirectory, daemonKeys } from '@/lib/daemon-api'
import { useDaemon } from '@/lib/daemon-context'
import { useI18n } from '@/lib/i18n'
import { cn } from '@/lib/utils'

export function DaemonFilePicker({
  root,
  workspaceLabel,
  selectionMode = 'attachment',
  returnFocus,
  onClose,
  onSelect,
}: {
  root: string | null
  workspaceLabel?: string
  selectionMode?: 'attachment' | 'file' | 'directory'
  returnFocus?: RefObject<HTMLElement | null>
  onClose: () => void
  onSelect: (absolutePath: string) => Promise<boolean>
}) {
  const { t } = useI18n()
  const { client, config, phase } = useDaemon()
  const list = useRef<VirtuosoHandle>(null)
  const [history, setHistory] = useState(() => [root])
  const [historyIndex, setHistoryIndex] = useState(0)
  const [filter, setFilter] = useState('')
  const [selectedPath, setSelectedPath] = useState<string | null>(null)
  const [submittingPath, setSubmittingPath] = useState<string | null>(null)
  const currentPath = history[historyIndex]!

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
  const resolvedPath = directory.data?.path ?? currentPath ?? ''
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
    setSelectedPath(selectionMode === 'directory' && currentPath ? currentPath : null)
    setFilter('')
  }, [currentPath, selectionMode])

  useEffect(() => {
    if (selectionMode === 'directory' && !selectedPath && directory.data?.path) {
      setSelectedPath(directory.data.path)
    }
  }, [directory.data?.path, selectedPath, selectionMode])

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
    if (path === undefined || submittingPath) return
    if (path === null) {
      if (currentPath === null) return
      setHistory((current) => [...current.slice(0, historyIndex + 1), null])
      setHistoryIndex(historyIndex + 1)
      return
    }
    if (currentPath && samePath(path, currentPath)) {
      if (selectionMode === 'directory') setSelectedPath(path)
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
    const onButton = Boolean((event.target as HTMLElement).closest('button'))
    if (event.key === 'Escape') {
      event.preventDefault()
      onClose()
    } else if (event.key === 'ArrowDown') {
      event.preventDefault()
      moveSelection(1)
    } else if (event.key === 'ArrowUp' && !event.metaKey) {
      event.preventDefault()
      moveSelection(-1)
    } else if (event.key === 'Enter' && !onButton) {
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
  const home = directory.data?.home
  const filesystemRoot = directory.data?.filesystem_root
  const showWorkspace = Boolean(root && workspaceLabel)
  const showHome = Boolean(home && (!root || !samePath(home, root)))
  const showFilesystem = Boolean(
    filesystemRoot
    && (!root || !samePath(filesystemRoot, root))
    && (!home || !samePath(filesystemRoot, home)),
  )

  return (
    <Dialog
      open
      onOpenChange={(open) => {
        if (!open) onClose()
      }}
    >
      <DialogContent
        aria-label={t(selectionMode === 'directory'
          ? 'file_picker.choose_project_folder'
          : 'file_picker.attach_from_daemon')}
        className="flex h-[min(510px,calc(100dvh-32px))] max-w-[720px] flex-col overflow-hidden rounded-[16px] bg-[var(--raised)] p-0"
        finalFocus={returnFocus}
        onKeyDown={handleKeyDown}
      >
        <header className="flex h-12 shrink-0 items-center border-b px-4 pr-12">
          <DialogTitle className="text-[13.5px] font-semibold">{dialogTitle}</DialogTitle>
        </header>

        <div className="flex min-h-0 flex-1">
          <aside className="w-[144px] shrink-0 border-r bg-background/35 p-2">
            <div className="px-2 pb-1 pt-1 text-[9.5px] font-semibold uppercase tracking-[0.06em] text-[var(--text-tertiary)]">
              {t('file_picker.locations')}
            </div>
            {showWorkspace && root && workspaceLabel && (
              <LocationButton
                active={samePath(resolvedPath, root)}
                label={workspaceLabel}
                title={root}
                onClick={() => visit(root)}
              />
            )}
            {(!root || showHome) && (
              <LocationButton
                active={currentPath === null || Boolean(home && samePath(resolvedPath, home))}
                label={t('file_picker.home')}
                title={home ?? t('file_picker.home')}
                onClick={() => visit(home ?? null)}
              />
            )}
            {showFilesystem && filesystemRoot && (
              <LocationButton
                active={samePath(resolvedPath, filesystemRoot)}
                label={t('file_picker.file_system')}
                title={filesystemRoot}
                onClick={() => visit(filesystemRoot)}
              />
            )}
          </aside>

          <main className="flex min-w-0 flex-1 flex-col bg-background/20">
            <div className="flex h-11 shrink-0 items-center gap-1 border-b px-2.5">
              <ToolbarButton
                label={t('file_picker.back')}
                disabled={historyIndex === 0 || Boolean(submittingPath)}
                icon="arrowLeft"
                onClick={() => setHistoryIndex((current) => current - 1)}
              />
              <ToolbarButton
                label={t('file_picker.forward')}
                disabled={historyIndex === history.length - 1 || Boolean(submittingPath)}
                icon="arrowRight"
                onClick={() => setHistoryIndex((current) => current + 1)}
              />
              <ToolbarButton
                label={t('file_picker.enclosing_folder')}
                disabled={!directory.data?.parent || Boolean(submittingPath)}
                icon="arrowUp"
                onClick={() => visit(directory.data?.parent)}
              />

              <div
                className="mx-1 flex min-w-0 flex-1 items-center gap-2 rounded-[7px] px-1.5 py-1"
                title={resolvedPath || undefined}
              >
                <WakuIcon className="size-[14px] text-[var(--text-tertiary)]" name="folder" />
                <span className="min-w-0 truncate text-[11.5px] text-[var(--text-secondary)]">
                  {resolvedPath || t('file_picker.loading_folder')}
                </span>
              </div>

              <label className="flex h-7 w-36 shrink-0 items-center gap-1.5 rounded-[7px] bg-foreground/[0.055] px-2 focus-within:ring-1 focus-within:ring-ring">
                <WakuIcon className="size-3 text-[var(--text-tertiary)]" name="search" />
                <input
                  autoFocus
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

            <div className="relative min-h-0 flex-1">
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
                  className="size-full py-1.5 outline-none"
                  computeItemKey={(_, entry) => entry.absolutePath}
                  data={visibleEntries}
                  fixedItemHeight={36}
                  increaseViewportBy={180}
                  itemContent={(_, entry) => {
                    const selectable = selectionMode !== 'directory' || entry.isDir
                    const selected = selectable && selectedPath === entry.absolutePath
                    return (
                      <div className="flex h-9 items-center px-2">
                        <button
                          aria-disabled={!selectable}
                          aria-selected={selected}
                          className={cn(
                            'flex h-8 w-full min-w-0 items-center gap-2 rounded-[7px] px-2.5 text-left text-[11.5px] outline-none',
                            selected
                              ? 'bg-accent text-foreground'
                              : 'hover:bg-accent/70 focus-visible:bg-accent focus-visible:ring-1 focus-visible:ring-ring',
                            !selectable && 'text-[var(--text-ghost)] hover:bg-transparent',
                          )}
                          role="option"
                          tabIndex={selectable ? 0 : -1}
                          type="button"
                          onClick={() => {
                            if (selectable) setSelectedPath(entry.absolutePath)
                          }}
                          onDoubleClick={() => activate(entry)}
                          onKeyDown={(event) => {
                            if (event.key !== 'Enter') return
                            event.preventDefault()
                            event.stopPropagation()
                            activate(entry)
                          }}
                        >
                          {entry.isDir ? (
                            <WakuIcon
                              className={cn(
                                'size-[15px] text-[var(--text-tertiary)]',
                                !selectable && 'text-[var(--text-ghost)]',
                              )}
                              name="folder"
                            />
                          ) : (
                            <FileTypeIcon
                              className={cn(
                                'size-[15px] opacity-75',
                                !selectable && 'opacity-35 grayscale',
                              )}
                              path={entry.absolutePath}
                            />
                          )}
                          <span className="min-w-0 flex-1 truncate">{entry.name}</span>
                          {selected && (
                            <WakuIcon className="size-3 text-[var(--text-tertiary)]" name="check" />
                          )}
                        </button>
                      </div>
                    )
                  }}
                  ref={list}
                  role="listbox"
                />
              )}
            </div>
          </main>
        </div>

        <footer className="flex h-[54px] shrink-0 items-center gap-2.5 border-t bg-background/25 px-3.5">
          <div
            className="flex min-w-0 flex-1 items-center gap-2 text-[11px] text-[var(--text-tertiary)]"
            title={selectedTarget ?? undefined}
          >
            <WakuIcon className="size-3.5 shrink-0" name={selectedEntry?.isDir === false ? 'file' : 'folder'} />
            <span className="truncate">{selectedTarget ?? t('file_picker.none')}</span>
          </div>
          <Button disabled={Boolean(submittingPath)} size="sm" type="button" variant="ghost" onClick={onClose}>
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
      </DialogContent>
    </Dialog>
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
        'flex h-8 w-full min-w-0 items-center gap-2 rounded-[7px] px-2 text-left text-[11.5px] text-[var(--text-secondary)] outline-none hover:bg-accent/70 focus-visible:ring-1 focus-visible:ring-ring',
        active && 'bg-accent text-foreground',
      )}
      title={title}
      type="button"
      onClick={onClick}
    >
      <WakuIcon className="size-[14px] text-[var(--text-tertiary)]" name="folder" />
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
      className="grid size-7 shrink-0 place-items-center rounded-[7px] text-[var(--text-secondary)] outline-none hover:bg-accent focus-visible:ring-1 focus-visible:ring-ring disabled:opacity-30"
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

function samePath(left: string, right: string): boolean {
  return left.replace(/[\\/]+$/, '') === right.replace(/[\\/]+$/, '')
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error)
}

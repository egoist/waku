export interface FilePanelTabShape {
  id: string
  surface: string
  selectedFile?: string | null
  dirty?: boolean
}

export interface FilePanelState<T extends FilePanelTabShape> {
  tabs: T[]
  activeId: string | null
}

export type TabNavigationKey = 'ArrowLeft' | 'ArrowRight' | 'Home' | 'End'

export type TreeNavigationKey = TabNavigationKey | 'ArrowUp' | 'ArrowDown'

export interface TreeNavigationRow {
  depth: number
  directory: boolean
  expanded: boolean
}

export interface TreeNavigationAction {
  index: number
  toggle?: boolean
}

export interface ReviewDiffFileShape {
  id: string
  path: string
  status: 'A' | 'B' | 'D' | 'M'
}

export function mergeReviewDiffFiles<T extends ReviewDiffFileShape>(
  parsed: readonly T[],
  numstat: string,
): ReviewDiffFileShape[] {
  const numstatFiles = new Map<string, 'B' | 'M'>()
  for (const line of numstat.split('\n')) {
    const firstTab = line.indexOf('\t')
    const secondTab = firstTab < 0 ? -1 : line.indexOf('\t', firstTab + 1)
    if (firstTab < 0 || secondTab < 0) continue
    const added = line.slice(0, firstTab)
    const deleted = line.slice(firstTab + 1, secondTab)
    const path = line.slice(secondTab + 1)
    if (!path) continue
    numstatFiles.set(path, added === '-' || deleted === '-' ? 'B' : 'M')
  }

  const merged: ReviewDiffFileShape[] = parsed.map((file) => (
    numstatFiles.get(file.path) === 'B' ? { ...file, status: 'B' } : file
  ))
  const parsedPaths = new Set(parsed.map((file) => file.path))
  for (const [path, status] of numstatFiles) {
    if (!parsedPaths.has(path)) merged.push({ id: `numstat:${path}`, path, status })
  }
  return merged
}

export function tabNavigationIndex(
  count: number,
  current: number,
  key: TabNavigationKey,
): number {
  if (count <= 0) return -1
  if (key === 'Home') return 0
  if (key === 'End') return count - 1
  if (key === 'ArrowLeft') return (current - 1 + count) % count
  return (current + 1) % count
}

export function treeNavigationAction(
  rows: readonly TreeNavigationRow[],
  current: number,
  key: TreeNavigationKey,
): TreeNavigationAction {
  if (!rows.length) return { index: -1 }
  if (key === 'Home') return { index: 0 }
  if (key === 'End') return { index: rows.length - 1 }
  if (key === 'ArrowDown') return { index: Math.min(rows.length - 1, current + 1) }
  if (key === 'ArrowUp') return { index: Math.max(0, current - 1) }

  const row = rows[current]
  if (!row) return { index: 0 }
  if (key === 'ArrowRight') {
    if (row.directory && !row.expanded) return { index: current, toggle: true }
    const child = rows[current + 1]
    return row.directory && child && child.depth > row.depth
      ? { index: current + 1 }
      : { index: current }
  }
  if (row.directory && row.expanded) return { index: current, toggle: true }
  for (let index = current - 1; index >= 0; index -= 1) {
    if (rows[index]!.depth < row.depth) return { index }
  }
  return { index: current }
}

export function openFileInPanel<T extends FilePanelTabShape>(
  current: FilePanelState<T>,
  file: string,
  originTabId: string | undefined,
  createTab: () => T,
): FilePanelState<T> {
  const origin = current.tabs.find((tab) => tab.id === (originTabId ?? current.activeId))
  const originIsFileSurface = origin?.surface === 'files' || origin?.surface === 'file'

  if (originIsFileSurface && origin) {
    if (origin.selectedFile === file) {
      return current.activeId === origin.id ? current : { ...current, activeId: origin.id }
    }

    if (!origin.dirty) {
      if (origin.surface === 'file') {
        const existing = current.tabs.find((tab) => (
          tab.id !== origin.id
          && tab.surface === 'file'
          && tab.selectedFile === file
        ))
        if (existing) {
          return {
            tabs: current.tabs.filter((tab) => tab.id !== origin.id),
            activeId: existing.id,
          }
        }
      }
      return {
        tabs: current.tabs.map((tab) => tab.id === origin.id
          ? { ...tab, selectedFile: file }
          : tab),
        activeId: origin.id,
      }
    }
  }

  const existing = current.tabs.find((tab) => (
    tab.surface === 'file'
    && tab.selectedFile === file
  ))
  if (existing) {
    return current.activeId === existing.id ? current : { ...current, activeId: existing.id }
  }

  const tab = createTab()
  return {
    tabs: [...current.tabs, tab],
    activeId: tab.id,
  }
}

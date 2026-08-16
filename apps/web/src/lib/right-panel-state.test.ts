import { describe, expect, test } from 'bun:test'
import {
  mergeReviewDiffFiles,
  openFileInPanel,
  tabNavigationIndex,
  treeNavigationAction,
  type FilePanelTabShape,
} from './right-panel-state'

describe('review diff file merge', () => {
  test('includes binary and numstat-only files', () => {
    expect(mergeReviewDiffFiles([
      { id: 'text', path: 'src/app.ts', status: 'M' },
      { id: 'binary', path: 'assets/logo.png', status: 'M' },
    ], [
      '4\t2\tsrc/app.ts',
      '-\t-\tassets/logo.png',
      '1\t0\tgenerated/new.ts',
    ].join('\n'))).toEqual([
      { id: 'text', path: 'src/app.ts', status: 'M' },
      { id: 'binary', path: 'assets/logo.png', status: 'B' },
      { id: 'numstat:generated/new.ts', path: 'generated/new.ts', status: 'M' },
    ])
  })

  test('ignores malformed numstat lines', () => {
    expect(mergeReviewDiffFiles([], 'not-numstat\n1\t2\t')).toEqual([])
  })
})

interface TestTab extends FilePanelTabShape {
  surface: 'files' | 'file' | 'terminal'
}

function fileTab(id: string, file: string): TestTab {
  return { id, surface: 'file', selectedFile: file }
}

function open(
  tabs: TestTab[],
  activeId: string,
  file: string,
  originTabId?: string,
) {
  return openFileInPanel(
    { tabs, activeId },
    file,
    originTabId,
    () => fileTab('new', file),
  )
}

describe('desktop file-tab behavior', () => {
  test('reuses a clean Files surface', () => {
    const state = open([
      { id: 'files', surface: 'files', selectedFile: 'old.ts' },
    ], 'files', 'new.ts')

    expect(state).toEqual({
      tabs: [{ id: 'files', surface: 'files', selectedFile: 'new.ts' }],
      activeId: 'files',
    })
  })

  test('opens a separate file surface instead of replacing a dirty editor', () => {
    const state = open([
      { id: 'files', surface: 'files', selectedFile: 'dirty.ts', dirty: true },
    ], 'files', 'new.ts')

    expect(state).toEqual({
      tabs: [
        { id: 'files', surface: 'files', selectedFile: 'dirty.ts', dirty: true },
        { id: 'new', surface: 'file', selectedFile: 'new.ts' },
      ],
      activeId: 'new',
    })
  })

  test('activates an existing file surface without duplicating it', () => {
    const state = open([
      { id: 'files', surface: 'files', selectedFile: 'dirty.ts', dirty: true },
      fileTab('existing', 'new.ts'),
    ], 'files', 'new.ts')

    expect(state.tabs).toHaveLength(2)
    expect(state.activeId).toBe('existing')
  })

  test('replaces a clean dedicated file surface', () => {
    const state = open([
      fileTab('current', 'old.ts'),
    ], 'current', 'new.ts')

    expect(state).toEqual({
      tabs: [fileTab('current', 'new.ts')],
      activeId: 'current',
    })
  })

  test('opens a dedicated file surface from a non-file tab', () => {
    const state = open([
      { id: 'terminal', surface: 'terminal' },
    ], 'terminal', 'new.ts')

    expect(state).toEqual({
      tabs: [
        { id: 'terminal', surface: 'terminal' },
        fileTab('new', 'new.ts'),
      ],
      activeId: 'new',
    })
  })
})

describe('right-panel tab keyboard navigation', () => {
  test('wraps with horizontal arrows', () => {
    expect(tabNavigationIndex(3, 0, 'ArrowLeft')).toBe(2)
    expect(tabNavigationIndex(3, 2, 'ArrowRight')).toBe(0)
  })

  test('moves directly to the first and last tabs', () => {
    expect(tabNavigationIndex(4, 2, 'Home')).toBe(0)
    expect(tabNavigationIndex(4, 1, 'End')).toBe(3)
  })
})

describe('virtual tree keyboard navigation', () => {
  const rows = [
    { depth: 0, directory: true, expanded: true },
    { depth: 1, directory: false, expanded: false },
    { depth: 0, directory: true, expanded: false },
  ]

  test('moves through visible rows and their bounds', () => {
    expect(treeNavigationAction(rows, 0, 'ArrowDown')).toEqual({ index: 1 })
    expect(treeNavigationAction(rows, 0, 'ArrowUp')).toEqual({ index: 0 })
    expect(treeNavigationAction(rows, 1, 'End')).toEqual({ index: 2 })
  })

  test('enters children, returns to parents, and toggles directories', () => {
    expect(treeNavigationAction(rows, 0, 'ArrowRight')).toEqual({ index: 1 })
    expect(treeNavigationAction(rows, 1, 'ArrowLeft')).toEqual({ index: 0 })
    expect(treeNavigationAction(rows, 2, 'ArrowRight')).toEqual({ index: 2, toggle: true })
    expect(treeNavigationAction(rows, 0, 'ArrowLeft')).toEqual({ index: 0, toggle: true })
  })
})

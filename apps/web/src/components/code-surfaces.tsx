import {
  parsePatchFiles,
  preloadHighlighter,
  type CodeViewItem,
  type SupportedLanguages,
  type ThemeTypes,
} from '@pierre/diffs'
import { Editor, type EditorOptions } from '@pierre/diffs/edit'
import {
  CodeView,
  EditProvider,
  File as DiffsFile,
  WorkerPoolContextProvider,
  type CodeViewHandle,
  useWorkerPool,
} from '@pierre/diffs/react'
import DiffWorkerUrl from '@pierre/diffs/worker/worker.js?worker&url'
import {
  forwardRef,
  useEffect,
  useImperativeHandle,
  useRef,
  useState,
  useSyncExternalStore,
  type ForwardedRef,
  type ReactNode,
  type PointerEvent as ReactPointerEvent,
} from 'react'
import { useI18n } from '../lib/i18n'
import { compactReviewPatch, createReviewDiffLoader } from '../lib/review-diff'

const nativeScrollbarCSS = `
  [data-code] {
    scrollbar-gutter: auto;
  }
`

const sharedOptions = {
  overflow: 'wrap' as const,
  preferredHighlighter: 'shiki-js' as const,
  unsafeCSS: nativeScrollbarCSS,
}

const commonCodeSurfaceLanguages: SupportedLanguages[] = [
  'text',
  'json',
  'markdown',
  'typescript',
  'tsx',
  'rust',
  'toml',
  'yaml',
  'css',
  'html',
  'zsh',
]

const workerPoolOptions = {
  poolSize: 3,
  workerFactory() {
    const worker = new Worker(DiffWorkerUrl, { type: 'module' })
    worker.addEventListener('error', markCodeSurfaceWorkersFailed, { once: true })
    return worker
  },
}

const workerHighlighterOptions = {
  langs: commonCodeSurfaceLanguages,
  lineDiffType: 'word-alt' as const,
  preferredHighlighter: 'shiki-js' as const,
  theme: { light: 'pierre-light' as const, dark: 'pierre-dark' as const },
}

let codeSurfaceWorkersFailed = false
const codeSurfaceWorkerFailureListeners = new Set<() => void>()

function markCodeSurfaceWorkersFailed() {
  if (codeSurfaceWorkersFailed) return
  codeSurfaceWorkersFailed = true
  for (const listener of codeSurfaceWorkerFailureListeners) listener()
}

let codeSurfacePreload: Promise<void> | undefined

export function preloadCodeSurfaces(): Promise<void> {
  codeSurfacePreload ??= preloadHighlighter({
    langs: commonCodeSurfaceLanguages,
    preferredHighlighter: 'shiki-js',
    themes: ['pierre-light', 'pierre-dark'],
  })
  return codeSurfacePreload
}

// Opening a code surface directly should share the same in-flight highlighter
// initialization as the idle preload started by the app shell.
void preloadCodeSurfaces().catch(() => {})

const fullHeightEditorCSS = `${nativeScrollbarCSS}

  :host, [data-file], [data-code] {
    min-height: 100%;
  }

  [data-code] {
    align-content: start;
  }
`

export interface DiffSurfaceFile {
  id: string
  path: string
  status: 'A' | 'B' | 'D' | 'M'
}

export interface CodeDiffSurfaceHandle {
  scrollToFile: (id: string) => void
}

export function CodeFileSurface({
  path,
  contents,
  cacheKey,
  editor,
  onChange,
  onEditor,
  onFocus,
}: {
  path: string
  contents: string
  cacheKey?: string
  editor?: Editor<undefined>
  onChange: (contents: string) => void
  onEditor?: (editor: Editor<undefined>) => void
  onFocus?: () => void
}) {
  const { t } = useI18n()
  const themeType = useResolvedTheme()
  const editorRef = useRef<Editor<undefined> | null>(null)
  const [createFileEditor] = useState(() => (options: EditorOptions<undefined>) => {
    const nextEditor = editor ?? new Editor<undefined>({ ...options, persistState: true })
    if (editor) nextEditor.setOptions({ ...options, persistState: true })
    editorRef.current = nextEditor
    onEditor?.(nextEditor)
    return nextEditor
  })
  // Diffs owns the live document while this surface is mounted. Keep the seed
  // on every keystroke. A path or disk-revision change remounts this surface.
  const [file] = useState(() => ({
    name: path,
    contents,
    cacheKey: cacheKey ?? `editor:${path}:${fastHash(contents)}`,
  }))
  const [renderReady, setRenderReady] = useState(false)
  const options = {
    ...sharedOptions,
    disableFileHeader: true,
    onPostRender: () => setRenderReady(true),
    themeType,
    unsafeCSS: fullHeightEditorCSS,
  }
  const editorOptions: EditorOptions<undefined> = {
    persistState: true,
    onChange: (nextFile) => onChange(nextFile.contents),
    onFocus,
  }
  const focusEditorFromCanvas = (event: ReactPointerEvent<HTMLDivElement>) => {
    if (event.button !== 0) return
    const hitDocument = event.nativeEvent.composedPath().some((target) => (
      target instanceof HTMLElement
      && target.dataset.line !== undefined
    ))
    if (hitDocument) return
    event.preventDefault()
    const editor = editorRef.current
    window.requestAnimationFrame(() => {
      editor?.focus({
        lineNumber: Number.MAX_SAFE_INTEGER,
        character: Number.MAX_SAFE_INTEGER,
        preventScroll: true,
      })
    })
  }

  return (
    <EditProvider createEditor={createFileEditor}>
      <div
        className="relative h-full min-h-0 min-w-0 flex-1 overflow-auto bg-card"
        onPointerDown={focusEditorFromCanvas}
      >
        <DiffsFile
          className="waku-code-editor waku-code-surface h-full min-h-full min-w-full bg-card"
          disableWorkerPool
          edit
          editorOptions={editorOptions}
          file={file}
          options={options}
        />
        {!renderReady && <CodeSurfaceLoading label={t('files.preparing_editor')} />}
      </div>
    </EditProvider>
  )
}

interface CodeDiffSurfaceProps {
  patch: string
  completeContext: boolean
  onFiles?: (files: DiffSurfaceFile[]) => void
}

export const CodeDiffSurface = forwardRef<CodeDiffSurfaceHandle, CodeDiffSurfaceProps>(
  function CodeDiffSurface(props, ref) {
    return (
      <WorkerPoolContextProvider
        highlighterOptions={workerHighlighterOptions}
        poolOptions={workerPoolOptions}
      >
        <CodeSurfaceWorkerBoundary>
          {(disableWorkerPool) => (
            <CodeDiffSurfaceContent
              {...props}
              disableWorkerPool={disableWorkerPool}
              forwardedRef={ref}
              key={`${Number(disableWorkerPool)}:${Number(props.completeContext)}:${props.patch.length}:${fastHash(props.patch)}`}
            />
          )}
        </CodeSurfaceWorkerBoundary>
      </WorkerPoolContextProvider>
    )
  },
)

function CodeDiffSurfaceContent({
  patch,
  completeContext,
  disableWorkerPool,
  onFiles,
  forwardedRef,
}: CodeDiffSurfaceProps & {
  disableWorkerPool: boolean
  forwardedRef: ForwardedRef<CodeDiffSurfaceHandle>
}) {
  const { t } = useI18n()
  const themeType = useResolvedTheme()
  const codeView = useRef<CodeViewHandle<undefined>>(null)
  const [renderReady, setRenderReady] = useState(false)
  const [model] = useState(() => {
    // The daemon hydrates review patches so the native client can expand any
    // hidden range locally. Parse a compact patch for the initial render, then
    // lazily expose the hydrated file contents when Diffs expands a gap.
    const cacheKey = `workspace-${fastHash(patch)}`
    const reviewPatch = compactReviewPatch(patch)
    const diffItems = parsePatchFiles(reviewPatch, `${cacheKey}:compact`, true)
      .flatMap((entry) => entry.files)
      .map((fileDiff, index) => ({
        id: `${fileDiff.name}-${index}`,
        type: 'diff' as const,
        fileDiff,
      }))
    const files: DiffSurfaceFile[] = diffItems.map((item) => ({
      id: item.id,
      path: item.fileDiff.name,
      status: item.fileDiff.type === 'new'
        ? 'A'
        : item.fileDiff.type === 'deleted'
          ? 'D'
          : 'M',
    }))
    return {
      cacheKey,
      files,
      items: diffItems satisfies CodeViewItem[],
      loadDiffFiles: completeContext ? createReviewDiffLoader(patch, cacheKey) : undefined,
    }
  })
  const options = {
    ...sharedOptions,
    diffIndicators: 'bars' as const,
    diffStyle: 'unified' as const,
    hunkSeparators: 'line-info-basic' as const,
    layout: { paddingTop: 0, paddingBottom: 0, gap: 0 },
    lineDiffType: 'word-alt' as const,
    loadDiffFiles: model.loadDiffFiles,
    stickyHeaders: true,
    themeType,
  }

  useEffect(() => {
    onFiles?.(model.files)
  }, [model.files, onFiles])

  useEffect(() => {
    setRenderReady(false)
    if (!model.items.length) {
      setRenderReady(true)
      return
    }
    let frame = 0
    const check = () => {
      if (codeView.current?.getInstance()?.getRenderedItems().length) {
        setRenderReady(true)
        return
      }
      frame = window.requestAnimationFrame(check)
    }
    frame = window.requestAnimationFrame(check)
    return () => window.cancelAnimationFrame(frame)
  }, [model.cacheKey])

  useImperativeHandle(forwardedRef, () => ({
    scrollToFile(id: string) {
      codeView.current?.scrollTo({ type: 'item', id, align: 'start', behavior: 'instant' })
    },
  }), [])

  return (
    <div className="relative size-full min-h-0 min-w-0 bg-background">
      {/* CodeView virtualizes both the multi-file item list and each file's
          rendered lines. Keep it as this surface's only scroll virtualizer. */}
      <CodeView
        className="waku-code-surface size-full overflow-auto bg-background"
        disableWorkerPool={disableWorkerPool}
        items={model.items}
        options={options}
        ref={codeView}
      />
      {!renderReady && <CodeSurfaceLoading label={t('diff.preparing_review')} />}
    </div>
  )
}

function CodeSurfaceWorkerBoundary({
  children,
}: {
  children: (disableWorkerPool: boolean) => ReactNode
}) {
  const pool = useWorkerPool()
  const disableWorkerPool = useSyncExternalStore(
    (listener) => {
      codeSurfaceWorkerFailureListeners.add(listener)
      return () => codeSurfaceWorkerFailureListeners.delete(listener)
    },
    () => codeSurfaceWorkersFailed,
    () => false,
  )

  useEffect(() => {
    if (!pool || disableWorkerPool) return
    let timeout: number | undefined
    const inspect = () => {
      const stats = pool.getStats()
      if (stats.workersFailed) {
        markCodeSurfaceWorkersFailed()
        return
      }
      window.clearTimeout(timeout)
      if (stats.managerState === 'initializing') {
        timeout = window.setTimeout(() => {
          if (!pool.isInitialized()) markCodeSurfaceWorkersFailed()
        }, 8_000)
      }
    }
    const unsubscribe = pool.subscribeToStatChanges(inspect)
    inspect()
    return () => {
      window.clearTimeout(timeout)
      unsubscribe()
    }
  }, [disableWorkerPool, pool])

  return children(disableWorkerPool)
}

function CodeSurfaceLoading({ label }: { label: string }) {
  return (
    <div
      aria-live="polite"
      className="pointer-events-none absolute inset-0 grid place-items-center bg-background text-[11px] text-[var(--text-tertiary)]"
      role="status"
    >
      {label}
    </div>
  )
}

function useResolvedTheme(): ThemeTypes {
  const [theme, setTheme] = useState<ThemeTypes>(() => resolvedTheme())

  useEffect(() => {
    const update = () => setTheme(resolvedTheme())
    const observer = new MutationObserver(update)
    observer.observe(document.documentElement, { attributes: true, attributeFilter: ['class'] })
    const media = window.matchMedia('(prefers-color-scheme: dark)')
    media.addEventListener('change', update)
    update()
    return () => {
      observer.disconnect()
      media.removeEventListener('change', update)
    }
  }, [])

  return theme
}

function resolvedTheme(): ThemeTypes {
  if (typeof document === 'undefined') return 'system'
  if (document.documentElement.classList.contains('dark')) return 'dark'
  return 'light'
}

function fastHash(value: string): string {
  return fastHashValue(value).toString(36)
}

function fastHashValue(value: string): number {
  let hash = 2_166_136_261
  for (let index = 0; index < value.length; index += 1) {
    hash ^= value.charCodeAt(index)
    hash = Math.imul(hash, 16_777_619)
  }
  return hash >>> 0
}

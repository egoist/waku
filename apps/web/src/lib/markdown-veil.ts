export const MARKDOWN_VEIL_EMA_SEED_MS = 160
export const MARKDOWN_VEIL_MIN_FADE_MS = 120
export const MARKDOWN_VEIL_MAX_FADE_MS = 400
const MARKDOWN_VEIL_GAP_CLAMP_MS = 1_000
const MARKDOWN_VEIL_MAX_CHUNKS = 32

export type MarkdownVeilChunk = {
  start: number
  end: number
  startedAt: number
  durationMs: number
}

export type MarkdownVeilState = {
  previous: string
  emaMs: number
  lastAppendAt?: number
  chunks: MarkdownVeilChunk[]
}

type HastPosition = {
  start: { offset?: number }
  end: { offset?: number }
}

export type HastNode = {
  type: string
  value?: string
  tagName?: string
  properties?: Record<string, unknown>
  children?: HastNode[]
  position?: HastPosition
}

export function createMarkdownVeilState(text = ''): MarkdownVeilState {
  return {
    previous: text,
    emaMs: MARKDOWN_VEIL_EMA_SEED_MS,
    chunks: [],
  }
}

function commonPrefix(left: string, right: string) {
  let offset = 0
  const length = Math.min(left.length, right.length)
  while (offset < length && left.charCodeAt(offset) === right.charCodeAt(offset)) offset += 1
  return offset
}

function fadeDuration(emaMs: number) {
  return Math.min(MARKDOWN_VEIL_MAX_FADE_MS, Math.max(MARKDOWN_VEIL_MIN_FADE_MS, emaMs * 3))
}

function fadeBoost(chunkCount: number) {
  return 1 + 0.3 * Math.max(0, chunkCount - 2)
}

function activeChunks(chunks: MarkdownVeilChunk[], now: number) {
  const boost = fadeBoost(chunks.length)
  return chunks.filter((chunk) => (now - chunk.startedAt) * boost < chunk.durationMs)
}

/**
 * Register the newly appended source range. Existing chunks keep their own
 * start time, allowing several provider deltas to dissolve concurrently.
 */
export function advanceMarkdownVeil(
  state: MarkdownVeilState,
  text: string,
  streaming: boolean,
  now: number,
) {
  if (!streaming) {
    state.previous = text
    state.chunks = []
    state.lastAppendAt = undefined
    return []
  }

  if (text !== state.previous) {
    const prefix = commonPrefix(state.previous, text)
    state.chunks = state.chunks.flatMap((chunk) => {
      const end = Math.min(chunk.end, prefix)
      return chunk.start < end ? [{ ...chunk, end }] : []
    })
    if (text.length > prefix) {
      if (state.lastAppendAt !== undefined) {
        const gap = Math.min(Math.max(0, now - state.lastAppendAt), MARKDOWN_VEIL_GAP_CLAMP_MS)
        state.emaMs = state.emaMs * 0.7 + gap * 0.3
      }
      state.lastAppendAt = now
      state.chunks.push({
        start: prefix,
        end: text.length,
        startedAt: now,
        durationMs: fadeDuration(state.emaMs),
      })
      if (state.chunks.length > MARKDOWN_VEIL_MAX_CHUNKS) {
        state.chunks.splice(0, state.chunks.length - MARKDOWN_VEIL_MAX_CHUNKS)
      }
    }
    state.previous = text
  }
  state.chunks = activeChunks(state.chunks, now)
  return state.chunks
}

function chunkStyle(chunk: MarkdownVeilChunk, now: number, chunkCount: number) {
  const duration = chunk.durationMs / fadeBoost(chunkCount)
  const elapsed = Math.min(Math.max(0, now - chunk.startedAt), duration)
  return [
    `--markdown-veil-duration:${duration}ms`,
    `--markdown-veil-delay:-${elapsed}ms`,
  ].join(';')
}

function splitTextNode(node: HastNode, chunks: MarkdownVeilChunk[], now: number): HastNode[] {
  const value = node.value ?? ''
  const start = node.position?.start.offset
  if (start === undefined || value.length === 0) return [node]
  const end = start + value.length
  const cuts = new Set([start, end])
  let intersects = false
  for (const chunk of chunks) {
    const from = Math.max(start, chunk.start)
    const to = Math.min(end, chunk.end)
    if (from < to) {
      intersects = true
      cuts.add(from)
      cuts.add(to)
    }
  }
  if (!intersects) return [node]
  const ordered = [...cuts].sort((left, right) => left - right)

  return ordered.slice(0, -1).flatMap((from, index) => {
    const to = ordered[index + 1]!
    const text: HastNode = {
      type: 'text',
      value: value.slice(from - start, to - start),
    }
    const chunk = chunks.find((candidate) => candidate.start <= from && to <= candidate.end)
    if (!chunk) return [text]
    return [{
      type: 'element',
      tagName: 'span',
      properties: {
        className: ['markdown-stream-veil'],
        style: chunkStyle(chunk, now, chunks.length),
      },
      children: [text],
    }]
  })
}

/** Rehype transform that wraps only active appended text ranges. */
export function applyMarkdownVeil(tree: HastNode, chunks: MarkdownVeilChunk[], now: number) {
  if (!chunks.length) return
  const visit = (node: HastNode) => {
    if (!node.children) return
    node.children = node.children.flatMap((child) => {
      if (child.type === 'text') return splitTextNode(child, chunks, now)
      visit(child)
      return [child]
    })
  }
  visit(tree)
}

export function markdownVeilPlugin(chunks: MarkdownVeilChunk[], now: number) {
  return () => (tree: HastNode) => applyMarkdownVeil(tree, chunks, now)
}

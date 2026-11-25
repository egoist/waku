/**
 * LaTeX-style math delimiters, normalized before the markdown parser runs.
 *
 * `remark-math` only knows `$…$` and `$$…$$`. A model that writes the
 * LaTeX-style `\[…\]` and `\(…\)` instead has its backslashes eaten by
 * CommonMark's ASCII-punctuation escape, so the delimiters are gone before the
 * math extension ever sees them and the formula renders as mangled text — the
 * `[` and `(` with the body left intact, because letters are not escapable.
 *
 * This pass rewrites those delimiters to the dollar form, and turns a display
 * formula that sits on its own line into the block form `remark-math` reads:
 * `$$x$$` on one line is text math to the extension, while the desktop's
 * parser reads the same source as a display block.
 *
 * Code spans and fenced code blocks are skipped: a `\[` in code is literal
 * text, and rewriting it would change what the code says.
 */

/** One math region in the source. */
type Region = {
  /** Offset of the region's opening delimiter. */
  start: number
  /** Offset just past the region's closing delimiter. */
  end: number
  /** The region's kind. */
  kind: 'inline' | 'display'
  /** Whether the region is the only thing on its line. */
  alone: boolean
}

/**
 * Rewrite the math delimiters in `source`.
 *
 * The formula itself is taken from the source verbatim, so the escapes inside
 * it (`\{`, `\%`) survive the trip.
 */
export function rewriteTexDelimiters(source: string): string {
  const regions = findRegions(source)
  if (!regions.length) return source
  let text = ''
  let cursor = 0
  for (const region of regions) {
    const body = source.slice(region.start + 2, region.end - 2)
    if (!body) continue
    text += source.slice(cursor, region.start)
    text += region.kind === 'inline' ? `$${body}$` : displayForm(region, body)
    cursor = region.end
  }
  return text + source.slice(cursor)
}

/**
 * The display form of a region.
 *
 * A formula alone on its line becomes the block form `remark-math` parses as
 * display math. Anywhere else it stays inline: the extension has no way to
 * read a display formula in the middle of a paragraph.
 */
function displayForm(region: Region, body: string): string {
  return region.alone ? `$$\n${body}\n$$` : `$$${body}$$`
}

/**
 * Find the math regions in `source`.
 *
 * Only the delimiters the markdown parser cannot read are collected. The
 * single-dollar form is left alone: `remark-math` already parses it, and
 * rewriting it would only risk changing what a paragraph says.
 */
function findRegions(source: string): Region[] {
  const regions: Region[] = []
  let index = 0
  while (index < source.length) {
    // A code fence opens at the start of a line.
    if (atLineStart(source, index)) {
      const fence = fenceAt(source, index)
      if (fence) {
        index = skipFence(source, index, fence.marker, fence.run)
        continue
      }
    }
    const char = source[index]
    if (char === '`') {
      index = skipCodeSpan(source, index)
      continue
    }
    if (char === '\\') {
      const next = source[index + 1]
      if (next === '\\') {
        // An escaped backslash is not the start of anything.
        index += 2
        continue
      }
      if (next !== '[' && next !== '(') {
        index += 1
        continue
      }
      const display = next === '['
      const closer = display ? ']' : ')'
      const close = findCloser(source, index + 2, closer)
      if (close < 0) {
        // An unclosed delimiter is ordinary text: the formula is still
        // streaming, and rewriting it would change the rendering of this
        // block when the closer arrives.
        index += 1
        continue
      }
      const end = close + 2
      const body = source.slice(index + 2, close)
      // A dollar in the body would end the math span early, and an empty body
      // is not a formula at all.
      if (!body || body.includes('$')) {
        index = end
        continue
      }
      regions.push({ start: index, end, kind: display ? 'display' : 'inline', alone: aloneOnLine(source, index, end) })
      index = end
      continue
    }
    if (char === '$' && source[index + 1] === '$') {
      const close = findDollarCloser(source, index + 2)
      if (close < 0) {
        index += 2
        continue
      }
      const end = close + 2
      const body = source.slice(index + 2, close)
      // A body that spans lines is already the block form; leave it alone.
      if (body && !body.includes('$') && !body.includes('\n')) {
        regions.push({ start: index, end, kind: 'display', alone: aloneOnLine(source, index, end) })
        index = end
        continue
      }
      index = end
      continue
    }
    index += 1
  }
  return regions
}

/** Whether the region between the two offsets is the only thing on its line. */
function aloneOnLine(source: string, start: number, end: number): boolean {
  const lineStart = source.lastIndexOf('\n', start - 1) + 1
  if (source.slice(lineStart, start).trim() !== '') return false
  const lineEnd = source.indexOf('\n', end)
  const rest = lineEnd < 0 ? source.slice(end) : source.slice(end, lineEnd)
  return rest.trim() === ''
}

/** Whether the offset sits at the start of a line. */
function atLineStart(source: string, index: number): boolean {
  return index === 0 || source[index - 1] === '\n'
}

/**
 * Find the closing delimiter of a LaTeX-style math region.
 *
 * Returns the offset of the backslash that opens the closer. A blank line
 * ends the search: a formula cannot span two paragraphs, and a region that
 * spans one must not change the rendering of an earlier block when its closer
 * finally arrives.
 */
function findCloser(source: string, from: number, closer: string): number {
  let index = from
  while (index < source.length) {
    const char = source[index]
    if (char === '\n') {
      let next = index + 1
      while (next < source.length && (source[next] === ' ' || source[next] === '\t')) next += 1
      if (next < source.length && source[next] === '\n') return -1
      index += 1
      continue
    }
    if (char === '`') {
      index = skipCodeSpan(source, index)
      continue
    }
    if (char === '\\') {
      if (source[index + 1] === closer) return index
      index += 2
      continue
    }
    index += 1
  }
  return -1
}

/** Find the closing `$$` of a display region. */
function findDollarCloser(source: string, from: number): number {
  let index = from
  while (index < source.length) {
    const char = source[index]
    if (char === '\\') {
      index += 2
      continue
    }
    if (char === '$' && source[index + 1] === '$') return index
    index += 1
  }
  return -1
}

/**
 * Skip an inline code span, returning the offset just past it.
 *
 * A backtick run with no matching run is literal text, so the scan carries on
 * after the run instead of swallowing the rest of the source.
 */
function skipCodeSpan(source: string, start: number): number {
  let run = 0
  while (source[start + run] === '`') run += 1
  let index = start + run
  while (index < source.length) {
    if (source[index] !== '`') {
      index += 1
      continue
    }
    let other = 0
    while (source[index + other] === '`') other += 1
    if (other === run) return index + other
    index += other
  }
  return start + run
}

/** The fence that opens at `lineStart`, as its marker and run length. */
function fenceAt(source: string, lineStart: number): { marker: string; run: number } | null {
  let cursor = lineStart
  let spaces = 0
  while (source[cursor] === ' ' && spaces < 4) {
    cursor += 1
    spaces += 1
  }
  if (spaces > 3) return null
  const marker = source[cursor]
  if (marker !== '`' && marker !== '~') return null
  let run = 0
  while (source[cursor + run] === marker) run += 1
  return run >= 3 ? { marker, run } : null
}

/** Skip a fenced code block, returning the offset just past its closing fence. */
function skipFence(source: string, start: number, marker: string, openRun: number): number {
  let index = start
  for (;;) {
    while (index < source.length && source[index] !== '\n') index += 1
    if (index >= source.length) return source.length
    index += 1
    let cursor = index
    while (source[cursor] === ' ') cursor += 1
    let run = 0
    while (source[cursor + run] === marker) run += 1
    if (run >= openRun) {
      while (cursor < source.length && source[cursor] !== '\n') cursor += 1
      return cursor
    }
  }
}

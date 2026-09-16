/**
 * Closes hanging inline markers in a *streaming* markdown tail.
 *
 * While a paragraph streams, an unclosed `**bold`, `*em`, `` `code ``,
 * `~~strike` or `[text](half-url` parses as literal text. When the closer
 * finally arrives the marker characters vanish and the run restyles, so wrap
 * points shift and the paragraph's tail visibly reflows — a jitter that lands
 * on every emphasis in a streamed response.
 *
 * Appending synthetic closers to the *display* parse keeps styling stable from
 * the moment content follows an opener. Only the display tree sees mended text;
 * the canonical text is untouched, so a marker that genuinely never closes
 * settles honestly — one flip when the response completes — instead of
 * jittering throughout.
 *
 * The scanner is deliberately approximate: it prefers "stable and close to the
 * final parse" over exact CommonMark delimiter resolution, because any
 * mid-stream misjudgement is repaired by the very next append or by the
 * settle. Cost is one pass over the tail, and `null` — zero further work —
 * whenever nothing hangs, which is the overwhelmingly common case.
 *
 * Port of the desktop's `src/md/mend.rs`; keep the two in behavioral lockstep.
 */

/** Destination for a link whose URL is still streaming. The renderer styles it
 * like a link but must not make it tappable. */
export const PENDING_LINK_URL = 'waku:pending-link';

/** Zero-width space appended to defuse a would-be setext underline. */
const ZERO_WIDTH_SPACE = '\u200B';

/** One unclosed emphasis-family delimiter run. */
interface OpenDelimiter {
  marker: string;
  /** Closers still owed. A short closer (`**a*`) decrements this. */
  owed: number;
  /** Char index just past the opening run: both the nesting order and the
   * "content must follow" guard compare against it. */
  openedAt: number;
}

interface CharSlot {
  /** UTF-16 offset of this code point in the source string. */
  offset: number;
  ch: string;
}

const WHITESPACE = /\s/u;
const ALPHANUMERIC = /[\p{L}\p{N}]/u;

/** Repair hanging inline markers. Returns `null` when nothing needs repair. */
export function closeHanging(text: string): string | null {
  const chars: CharSlot[] = [];
  {
    let offset = 0;
    for (const ch of text) {
      chars.push({ offset, ch });
      offset += ch.length;
    }
  }
  const at = (index: number): string | undefined => chars[index]?.ch;

  const delimiters: OpenDelimiter[] = [];
  const brackets: number[] = [];
  // Open inline code span: [backtick run length, char index of its content].
  let code: [number, number] | null = null;
  let lastContent: number | null = null;
  // Char index of the `]` whose `](…` URL runs off the end of the tail.
  let pendingUrl: number | null = null;

  let index = 0;
  while (index < chars.length) {
    const ch = chars[index]!.ch;

    if (code === null && ch === '\\') {
      // An escape and its escapee are both literal, but the escapee is
      // still content that can justify closing an opener.
      if (index + 1 < chars.length) {
        lastContent = index + 1;
      }
      index += 2;
      continue;
    }

    if (ch === '`') {
      const run = runLength(chars, index);
      if (code !== null && run === code[0]) {
        // A span closes only on a run of exactly the opening length.
        // Its closing backticks are content: an emphasis closer owed
        // from before the span must land after them, not inside it.
        code = null;
        lastContent = index + run - 1;
      } else if (code !== null) {
        lastContent = index + run - 1;
      } else {
        code = [run, index + run];
      }
      index += run;
      continue;
    }

    if (code !== null) {
      lastContent = index;
      index += 1;
      continue;
    }

    if (ch === '*' || ch === '_' || ch === '~') {
      const run = runLength(chars, index);
      lastContent = scanDelimiter(delimiters, ch, run, index, lastContent, at);
      index += run;
    } else if (ch === '[') {
      brackets.push(index);
      index += 1;
    } else if (ch === ']') {
      const open = brackets.pop();
      if (open !== undefined) {
        // Emphasis opened inside a completed `[…]` and never closed
        // there stays literal, as the final parse will also decide.
        for (let cut = delimiters.length - 1; cut >= 0; cut -= 1) {
          if (delimiters[cut]!.openedAt >= open) delimiters.splice(cut, 1);
        }
        if (at(index + 1) === '(') {
          // Consume the URL through its balanced `)`.
          let scan = index + 2;
          let depth = 0;
          for (;;) {
            const next = at(scan);
            if (next === undefined) {
              pendingUrl = index;
              break;
            }
            if (next === '(') depth += 1;
            else if (next === ')' && depth === 0) break;
            else if (next === ')') depth -= 1;
            scan += 1;
          }
          if (pendingUrl !== null) break;
          lastContent = scan;
          index = scan + 1;
          continue;
        }
      }
      lastContent = index;
      index += 1;
    } else if (WHITESPACE.test(ch)) {
      index += 1;
    } else {
      lastContent = index;
      index += 1;
    }
  }

  // Closers must sit immediately after the last content character: a `**`
  // preceded by whitespace is left-flanking, so it would open a *new* run
  // rather than close the hanging one, and the repair would never converge.
  const contentEnd = lastContent !== null
    ? chars[lastContent]!.offset + chars[lastContent]!.ch.length
    : text.length;

  if (pendingUrl !== null) {
    // The URL never renders, so a bare `[text` and a half-typed
    // `[text](http` mend identically: the link text shows styled at once
    // and the settling URL cannot collapse the line.
    const cut = chars[pendingUrl]!.offset;
    const closers = closeDelimiters(delimiters, lastContent, pendingUrl);
    const head = Math.min(contentEnd, cut);
    return `${text.slice(0, head)}${closers}${text.slice(head, cut)}](${PENDING_LINK_URL})`;
  }

  let closers = '';
  if (code !== null && lastContent !== null && lastContent >= code[1]) {
    closers += '`'.repeat(code[0]);
  }
  closers += closeDelimiters(delimiters, lastContent, chars.length);
  const bracket = brackets.at(-1);
  if (bracket !== undefined && lastContent !== null && lastContent > bracket) {
    closers += `](${PENDING_LINK_URL})`;
  }

  const setextGuard = needsSetextGuard(text);
  if (!closers && !setextGuard) {
    return null;
  }

  let mended = `${text.slice(0, contentEnd)}${closers}${text.slice(contentEnd)}`;
  if (setextGuard) mended += ZERO_WIDTH_SPACE;
  return mended;
}

/** Append closers for every still-open delimiter, innermost first, skipping any
 * opener that has no content after it yet (`**` alone must stay literal, not
 * become an empty bold). */
function closeDelimiters(
  delimiters: OpenDelimiter[],
  lastContent: number | null,
  limit: number,
): string {
  let out = '';
  for (let position = delimiters.length - 1; position >= 0; position -= 1) {
    const delimiter = delimiters[position]!;
    if (delimiter.openedAt >= limit) continue;
    if (lastContent === null || lastContent < delimiter.openedAt) continue;
    out += delimiter.marker.repeat(delimiter.owed);
  }
  return out;
}

function runLength(chars: CharSlot[], start: number): number {
  const marker = chars[start]!.ch;
  let length = 0;
  while (start + length < chars.length && chars[start + length]!.ch === marker) {
    length += 1;
  }
  return length;
}

/** Classify one emphasis-family run as an opener or a closer and update the
 * open-delimiter stack. Returns the updated last-content index. */
function scanDelimiter(
  delimiters: OpenDelimiter[],
  marker: string,
  run: number,
  index: number,
  lastContent: number | null,
  at: (index: number) => string | undefined,
): number | null {
  // A lone `~` is literal in GFM: strikethrough needs `~~`.
  if (marker === '~' && run < 2) {
    return index + run - 1;
  }

  const before = index > 0 ? at(index - 1) : undefined;
  const after = at(index + run);
  const canClose = before !== undefined && !WHITESPACE.test(before);
  const canOpen = after !== undefined && !WHITESPACE.test(after);

  if (canClose) {
    let position = -1;
    for (let scan = delimiters.length - 1; scan >= 0; scan -= 1) {
      if (delimiters[scan]!.marker === marker) {
        position = scan;
        break;
      }
    }
    if (
      position >= 0 &&
      lastContent !== null &&
      lastContent >= delimiters[position]!.openedAt
    ) {
      const owed = delimiters[position]!.owed;
      if (run >= owed) {
        // Fully closed; any surplus markers are rare enough to ignore.
        delimiters.length = position;
      } else {
        // A half-streamed closer (`**a*`): we still owe the remainder.
        delimiters[position]!.owed = owed - run;
        delimiters.length = position + 1;
      }
      return index + run - 1;
    }
  }

  // `_` does not open inside a word, matching CommonMark's intraword rule.
  if (marker === '_' && before !== undefined && ALPHANUMERIC.test(before)) {
    return index + run - 1;
  }

  if (canOpen) {
    delimiters.push({ marker, owed: run, openedAt: index + run });
    return lastContent;
  }
  return index + run - 1;
}

/** A final line of only `-` or `=` directly under a text line is a setext
 * heading underline, so a streaming list item flashes its whole paragraph as a
 * heading for one chunk. True when that flash is imminent. */
function needsSetextGuard(text: string): boolean {
  if (!text || text.endsWith('\n')) return false;
  const lines = text.split('\n');
  const last = lines.at(-1)!;
  const trimmed = last.trimEnd();
  if (
    !trimmed ||
    !(/^-+$/.test(trimmed) || /^=+$/.test(trimmed))
  ) {
    return false;
  }
  const previous = lines.at(-2)?.trim();
  return Boolean(
    previous && !['-', '=', '#', '>', '`'].includes(previous[0]!),
  );
}

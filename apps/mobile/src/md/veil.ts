/**
 * Paint-only fade for newly appended streaming Markdown text.
 *
 * The complete text enters layout immediately. Only the colors of newly
 * appended ranges animate, so the fade cannot change shaping, wrapping,
 * selection offsets, or row height.
 *
 * Port of the desktop's `src/md/veil.rs`: fades are keyed per rendered text
 * element ordinal and tracked against that element's *visible* text, so a
 * Markdown reparse that swallows delimiter characters re-veils only the
 * changed tail instead of flashing the whole block.
 */

export const VEIL_EMA_SEED_MS = 160;
export const VEIL_MIN_FADE_MS = 120;
export const VEIL_MAX_FADE_MS = 400;
export const VEIL_CURVE_POW = 1.6;
const VEIL_GAP_CLAMP_MS = 1_000;

interface Chunk {
  start: number;
  end: number;
  startedAt: number;
  durationMs: number;
}

/** One faded range of an element's visible text: [start, end, opacity]. */
export type VeilSpan = readonly [start: number, end: number, opacity: number];

export function veilOpacity(progress: number): number {
  const clamped = Math.min(1, Math.max(0, progress));
  return 1 - (1 - clamped) ** VEIL_CURVE_POW;
}

export function veilDurationMs(emaMs: number): number {
  return Math.min(VEIL_MAX_FADE_MS, Math.max(VEIL_MIN_FADE_MS, emaMs * 3));
}

export function veilBoost(activeChunks: number): number {
  return 1 + 0.3 * Math.max(0, activeChunks - 2);
}

export function veilEmaNext(emaMs: number, gapMs: number): number {
  return emaMs * 0.7 + Math.min(gapMs, VEIL_GAP_CLAMP_MS) * 0.3;
}

function commonPrefix(a: string, b: string): number {
  let prefix = 0;
  const length = Math.min(a.length, b.length);
  while (prefix < length && a.charCodeAt(prefix) === b.charCodeAt(prefix)) {
    prefix += 1;
  }
  // Never split a surrogate pair: a boundary inside one would render both
  // halves as broken glyphs while the tail fades.
  const trailing = b.charCodeAt(prefix);
  if (prefix > 0 && trailing >= 0xdc00 && trailing <= 0xdfff) {
    prefix -= 1;
  }
  return prefix;
}

class ElementVeil {
  previous = '';
  private chunks: Chunk[] = [];
  private emaMs = VEIL_EMA_SEED_MS;
  private lastAppend: number | null = null;

  seed(text: string) {
    this.previous = text;
  }

  advance(text: string, now: number): VeilSpan[] {
    if (text !== this.previous) {
      // A streaming Markdown reparse can replace delimiter characters
      // with styled text. Preserve the common prefix and re-veil only
      // the changed tail instead of flashing the whole block.
      const prefix = commonPrefix(this.previous, text);
      this.chunks = this.chunks.flatMap((chunk) => {
        const end = Math.min(chunk.end, prefix);
        return chunk.start < end ? [{ ...chunk, end }] : [];
      });
      if (text.length > prefix) {
        if (this.lastAppend !== null) {
          this.emaMs = veilEmaNext(this.emaMs, Math.max(0, now - this.lastAppend));
        }
        this.lastAppend = now;
        this.chunks.push({
          start: prefix,
          end: text.length,
          startedAt: now,
          durationMs: veilDurationMs(this.emaMs),
        });
      }
      this.previous = text;
    }

    const pruneBoost = veilBoost(this.chunks.length);
    this.chunks = this.chunks.filter(
      (chunk) => Math.max(0, now - chunk.startedAt) * pruneBoost < chunk.durationMs,
    );
    const boost = veilBoost(this.chunks.length);
    return this.chunks.map((chunk) => {
      const elapsed = Math.max(0, now - chunk.startedAt);
      const progress = Math.min(1, Math.max(0, (elapsed * boost) / chunk.durationMs));
      return [chunk.start, chunk.end, veilOpacity(progress)] as const;
    });
  }

  isFading(): boolean {
    return this.chunks.length > 0;
  }
}

/**
 * Fade state for one Markdown body, keyed by the renderer's stable text
 * element ordinal.
 */
export class RowVeil {
  private elements = new Map<number, ElementVeil>();
  private seenThisFrame = new Set<number>();
  private seeding: boolean;

  /** `seeded` adopts existing content at full opacity on the first render.
   * Used when attaching to a response that was already streaming. */
  constructor(seeded = false) {
    this.seeding = seeded;
  }

  beginFrame() {
    this.seenThisFrame.clear();
  }

  finishSeeding() {
    this.seeding = false;
  }

  finishFrame() {
    for (const element of this.elements.keys()) {
      if (!this.seenThisFrame.has(element)) this.elements.delete(element);
    }
    this.finishSeeding();
  }

  advance(element: number, text: string, now: number): VeilSpan[] {
    this.seenThisFrame.add(element);
    if (this.seeding && !this.elements.has(element)) {
      const veil = new ElementVeil();
      veil.seed(text);
      this.elements.set(element, veil);
      return [];
    }
    let veil = this.elements.get(element);
    if (!veil) {
      veil = new ElementVeil();
      this.elements.set(element, veil);
    }
    return veil.advance(text, now);
  }

  isFading(): boolean {
    for (const element of this.elements.values()) {
      if (element.isFading()) return true;
    }
    return false;
  }
}

/**
 * Split one leaf text run at veil boundaries. `start` is the run's offset in
 * the element's flattened text. Returns [sliceStart, sliceEnd, opacity]
 * pieces covering the run exactly; opacity 1 means unveiled.
 */
export function splitRunAtSpans(
  start: number,
  length: number,
  spans: readonly VeilSpan[],
): Array<readonly [number, number, number]> {
  const end = start + length;
  if (!spans.length || length === 0) return [[start, end, 1]];
  const cuts = new Set([start, end]);
  for (const [from, to] of spans) {
    if (from > start && from < end) cuts.add(from);
    if (to > start && to < end) cuts.add(to);
  }
  const ordered = [...cuts].sort((a, b) => a - b);
  return ordered.slice(0, -1).map((pieceStart, index) => {
    const pieceEnd = ordered[index + 1]!;
    const span = spans.find(([from, to]) => from <= pieceStart && pieceEnd <= to);
    return [pieceStart, pieceEnd, span && span[2] < 1 ? span[2] : 1] as const;
  });
}

/**
 * Scroll model for the inverted transcript.
 *
 * The transcript renders newest-first inside a scale(-1) ScrollView, so the
 * native content offset 0 *is* the bottom. Following a stream is therefore
 * not a scroll at all: appended text lands at native y=0 and the viewport
 * stays glued to it inside the same layout pass. Everything here is the
 * bookkeeping around that — when the reader has left the tail, when to offer
 * the jump button, and when to mount more history above.
 *
 * All distances are in native (unflipped) coordinates: `offset` grows as the
 * reader scrolls up into history.
 */

/** A reader resting within this band of the bottom is re-seated on the
 * next content growth (or when their gesture ends); beyond it they are
 * reading and the transcript holds their line. */
export const STICK_THRESHOLD = 64;

/** Hysteresis for the jump-to-latest button. */
export const JUMP_SHOW_DISTANCE = 240;
export const JUMP_HIDE_DISTANCE = 120;

/** Pipeline rows (messages / tool groups / folds) mounted on open, and the
 * step by which the window grows toward older history. */
export const INITIAL_TAIL_ROWS = 40;
export const TAIL_ROWS_STEP = 40;

/** Mount more history once less than this many viewports remain above. */
export const EXTEND_LEAD_VIEWPORTS = 1.5;

export interface ScrollMetrics {
  offset: number;
  contentHeight: number;
  viewportHeight: number;
}

/** Content still hidden beyond the visual top of the viewport. */
export function distanceToTop(metrics: ScrollMetrics): number {
  return metrics.contentHeight - metrics.viewportHeight - metrics.offset;
}

/** True when transcript content sits under the floating header. */
export function underHeader(metrics: ScrollMetrics): boolean {
  return distanceToTop(metrics) > 4;
}

/** True when the reader is off the bottom but still inside the stick band. */
export function withinStickBand(offset: number): boolean {
  return offset > 0.5 && offset <= STICK_THRESHOLD;
}

/**
 * The stick band judged by the reader's own displacement. While a finger is
 * down the transcript anchors to history, so a stream that grows during the
 * touch leaves the reader further off the tail than they moved themselves;
 * `intent` is the release offset minus that compensated growth. A reader who
 * only peeked is re-seated even if the stream ran 300 px underneath them.
 */
export function peekedWithinBand(offset: number, intent: number): boolean {
  return offset > 0.5 && intent <= STICK_THRESHOLD;
}

/** True when the reader has left the tail: from here on the transcript
 * anchors to settled history rather than to the bottom. */
export function offTail(offset: number): boolean {
  return offset > STICK_THRESHOLD;
}

/** Jump-button visibility with hysteresis so it never flickers at a band edge. */
export function jumpVisible(offset: number, wasVisible: boolean): boolean {
  if (wasVisible) return offset > JUMP_HIDE_DISTANCE;
  return offset > JUMP_SHOW_DISTANCE;
}

/** True when the reader is close enough to the top of the mounted window
 * that older rows should be mounted now. Also true while the window is
 * shorter than the viewport, so a sparse tail fills the screen. */
export function shouldExtendTail(metrics: ScrollMetrics, hasEarlier: boolean): boolean {
  if (!hasEarlier) return false;
  if (metrics.viewportHeight <= 0) return false;
  return distanceToTop(metrics) < metrics.viewportHeight * EXTEND_LEAD_VIEWPORTS;
}

export function initialWindowStart(pipelineLength: number): number {
  return Math.max(0, pipelineLength - INITIAL_TAIL_ROWS);
}

export function extendedWindowStart(start: number): number {
  return Math.max(0, start - TAIL_ROWS_STEP);
}

/**
 * Native child index of a transcript row. Children are mounted newest-first
 * after `leadingChildren` fixed views (the working strip), so the last row
 * of the array is the first row child.
 */
export function nativeChildIndex(
  rowIndex: number,
  rowCount: number,
  leadingChildren = 1,
): number {
  return leadingChildren + (rowCount - 1 - rowIndex);
}

/**
 * Native child index of the first row that cannot grow while a turn runs:
 * everything after the strip and the running turn's own rows. Anchoring the
 * viewport there keeps a reader's line fixed even when the growth happens
 * inside a single tall row (a long reasoning block, a streaming list), which
 * the native anchor cannot see into. With no running turn nothing grows and
 * the strip (index 0) is the anchor.
 */
export function liveSkipIndex(
  rows: readonly { turnId: string | null }[],
  runningTurnId: string | null,
  leadingChildren = 1,
): number {
  if (!runningTurnId) return 0;
  let live = 0;
  for (let ix = rows.length - 1; ix >= 0 && rows[ix]!.turnId === runningTurnId; ix -= 1) {
    live += 1;
  }
  return leadingChildren + live;
}

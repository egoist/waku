import { describe, expect, test } from 'bun:test';

import {
  EXTEND_LEAD_VIEWPORTS,
  INITIAL_TAIL_ROWS,
  JUMP_HIDE_DISTANCE,
  JUMP_SHOW_DISTANCE,
  TAIL_ROWS_STEP,
  distanceToTop,
  extendedWindowStart,
  initialWindowStart,
  jumpVisible,
  liveSkipIndex,
  nativeChildIndex,
  offTail,
  peekedWithinBand,
  shouldExtendTail,
  STICK_THRESHOLD,
  underHeader,
  withinStickBand,
} from './transcript-scroll';

describe('inverted transcript scroll model', () => {
  test('measures hidden content above the viewport in native coordinates', () => {
    expect(distanceToTop({ offset: 0, contentHeight: 2_000, viewportHeight: 800 })).toBe(1_200);
    expect(distanceToTop({ offset: 1_200, contentHeight: 2_000, viewportHeight: 800 })).toBe(0);
    // Short transcripts never reach under the header.
    expect(distanceToTop({ offset: 0, contentHeight: 300, viewportHeight: 800 })).toBe(-500);
  });

  test('reports content under the header only when it overflows the top', () => {
    expect(underHeader({ offset: 0, contentHeight: 300, viewportHeight: 800 })).toBe(false);
    expect(underHeader({ offset: 0, contentHeight: 900, viewportHeight: 800 })).toBe(true);
    expect(underHeader({ offset: 100, contentHeight: 900, viewportHeight: 800 })).toBe(false);
  });

  test('jump button uses hysteresis around the show and hide bands', () => {
    expect(jumpVisible(JUMP_SHOW_DISTANCE, false)).toBe(false);
    expect(jumpVisible(JUMP_SHOW_DISTANCE + 1, false)).toBe(true);
    expect(jumpVisible(JUMP_SHOW_DISTANCE - 40, true)).toBe(true);
    expect(jumpVisible(JUMP_HIDE_DISTANCE, true)).toBe(false);
    expect(jumpVisible(0, true)).toBe(false);
  });

  test('extends the tail window ahead of the reader and never past the start', () => {
    const viewport = 800;
    const lead = viewport * EXTEND_LEAD_VIEWPORTS;
    expect(shouldExtendTail({ offset: 0, contentHeight: 5_000, viewportHeight: viewport }, true)).toBe(false);
    expect(shouldExtendTail({ offset: 5_000 - viewport - lead + 1, contentHeight: 5_000, viewportHeight: viewport }, true)).toBe(true);
    expect(shouldExtendTail({ offset: 4_000, contentHeight: 5_000, viewportHeight: viewport }, false)).toBe(false);
    // A window shorter than the viewport fills itself before any scroll.
    expect(shouldExtendTail({ offset: 0, contentHeight: 300, viewportHeight: viewport }, true)).toBe(true);
    expect(shouldExtendTail({ offset: 0, contentHeight: 300, viewportHeight: 0 }, true)).toBe(false);
  });

  test('window starts at the tail and grows toward history in steps', () => {
    expect(initialWindowStart(10)).toBe(0);
    expect(initialWindowStart(INITIAL_TAIL_ROWS + 25)).toBe(25);
    expect(extendedWindowStart(25)).toBe(Math.max(0, 25 - TAIL_ROWS_STEP));
    expect(extendedWindowStart(TAIL_ROWS_STEP + 3)).toBe(3);
    expect(extendedWindowStart(0)).toBe(0);
  });

  test('stick band and off-tail split at the threshold', () => {
    expect(withinStickBand(0)).toBe(false);
    expect(withinStickBand(1)).toBe(true);
    expect(withinStickBand(STICK_THRESHOLD)).toBe(true);
    expect(withinStickBand(STICK_THRESHOLD + 1)).toBe(false);
    expect(offTail(STICK_THRESHOLD)).toBe(false);
    expect(offTail(STICK_THRESHOLD + 1)).toBe(true);
  });

  test('a peek is judged by the reader’s own displacement, not the stream’s', () => {
    // Moved 23 px, but 330 px streamed in underneath during the touch.
    expect(peekedWithinBand(353, 23)).toBe(true);
    // Flung 400 px on their own: reading, leave them.
    expect(peekedWithinBand(400, 400)).toBe(false);
    // Tapped without moving while 40 px streamed in: still seated in intent.
    expect(peekedWithinBand(40, 0)).toBe(true);
    // Already at the bottom: nothing to do.
    expect(peekedWithinBand(0, 0)).toBe(false);
  });

  test('anchors past the strip and every row of the running turn', () => {
    const rows = [
      { turnId: 't1' }, { turnId: 't1' }, { turnId: 't2' }, { turnId: 't2' }, { turnId: 't2' },
    ];
    // rows mount as [strip, t2, t2, t2, t1, t1, sentinel]: first t1 row is child 4
    expect(liveSkipIndex(rows, 't2')).toBe(4);
    expect(liveSkipIndex(rows, 't1')).toBe(1);
    expect(liveSkipIndex(rows, null)).toBe(0);
    // A first turn owns every row: the sentinel after the last row anchors.
    expect(liveSkipIndex([{ turnId: 't1' }, { turnId: 't1' }], 't1')).toBe(3);
  });

  test('maps row order to newest-first native children after the strip', () => {
    // rows [a, b, c] mount as [strip, c, b, a, sentinel]
    expect(nativeChildIndex(2, 3)).toBe(1);
    expect(nativeChildIndex(0, 3)).toBe(3);
    expect(nativeChildIndex(1, 3, 2)).toBe(3);
  });
});

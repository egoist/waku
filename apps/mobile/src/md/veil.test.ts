import { describe, expect, test } from 'bun:test';

import {
  RowVeil,
  splitRunAtSpans,
  veilBoost,
  veilDurationMs,
  veilOpacity,
} from './veil';

describe('markdown streaming veil', () => {
  test('appended chunks fade once and independently', () => {
    const veil = new RowVeil();
    expect(veil.advance(0, 'one ', 0)).toEqual([[0, 4, 0]]);
    const spans = veil.advance(0, 'one two', 100);
    expect(spans.map(([start, end]) => [start, end])).toEqual([[0, 4], [4, 7]]);
    expect(spans[0]![2]).toBeGreaterThan(spans[1]![2]);
    expect(veil.advance(0, 'one two', 600)).toEqual([]);
    expect(veil.advance(0, 'one two', 700)).toEqual([]);
  });

  test('seeded rows do not refade existing content', () => {
    const veil = new RowVeil(true);
    expect(veil.advance(0, 'already here', 0)).toEqual([]);
    veil.finishSeeding();
    expect(veil.advance(0, 'already here plus', 100)).toEqual([[12, 17, 0]]);
  });

  test('markdown rewrites keep the common prefix', () => {
    const veil = new RowVeil();
    veil.advance(0, 'intro **bol', 0);
    const spans = veil.advance(0, 'intro bold', 100);
    expect(spans[0]!.slice(0, 2)).toEqual([0, 6]);
    expect(spans[1]!.slice(0, 2)).toEqual([6, 10]);
  });

  test('frame lifecycle prunes elements that disappeared', () => {
    const veil = new RowVeil();
    veil.beginFrame();
    veil.advance(0, 'kept', 0);
    veil.advance(1, 'gone', 0);
    veil.finishFrame();
    veil.beginFrame();
    veil.advance(0, 'kept', 100);
    veil.finishFrame();
    // Element 1 was pruned; reappearing text fades in as fresh content.
    expect(veil.advance(1, 'gone', 200)).toEqual([[0, 4, 0]]);
  });

  test('cadence curve matches the desktop veil', () => {
    expect(veilDurationMs(160)).toBe(400);
    expect(veilDurationMs(30)).toBe(120);
    expect(veilBoost(2)).toBe(1);
    expect(veilBoost(3)).toBeCloseTo(1.3);
    expect(veilOpacity(0)).toBe(0);
    expect(veilOpacity(1)).toBe(1);
    expect(veilOpacity(0.5)).toBeGreaterThan(0.5);
  });

  test('splits runs without changing covered lengths', () => {
    const pieces = splitRunAtSpans(0, 10, [[2, 8, 0.5]]);
    expect(pieces).toEqual([[0, 2, 1], [2, 8, 0.5], [8, 10, 1]]);
    const total = pieces.reduce((sum, [start, end]) => sum + (end - start), 0);
    expect(total).toBe(10);
    // A run fully inside a span keeps one piece at the span's opacity.
    expect(splitRunAtSpans(3, 2, [[2, 8, 0.25]])).toEqual([[3, 5, 0.25]]);
    // No spans: one unveiled piece.
    expect(splitRunAtSpans(4, 3, [])).toEqual([[4, 7, 1]]);
  });

  test('never splits a surrogate pair at a chunk boundary', () => {
    const veil = new RowVeil();
    veil.advance(0, 'aa', 0);
    // The next append starts mid-emoji only if the prefix landed inside the
    // pair; the guard walks it back to the pair start.
    const spans = veil.advance(0, 'aa🎉', 100);
    expect(spans.at(-1)!.slice(0, 2)).toEqual([2, 4]);
  });
});

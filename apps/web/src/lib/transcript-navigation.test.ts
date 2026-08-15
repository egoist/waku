import { describe, expect, test } from 'bun:test'
import {
  activeNavigationTurn,
  firstVisibleTranscriptItem,
} from './transcript-navigation'

describe('transcript navigation', () => {
  test('finds the first item inside the real viewport, not the overscanned range', () => {
    const rendered = [
      { index: 4, offset: 100, size: 80 },
      { index: 5, offset: 180, size: 120 },
      { index: 6, offset: 300, size: 90 },
      { index: 7, offset: 390, size: 70 },
    ]

    expect(firstVisibleTranscriptItem(rendered, 0)).toBeNull()
    expect(firstVisibleTranscriptItem(rendered, 179)).toBe(4)
    expect(firstVisibleTranscriptItem(rendered, 180)).toBe(5)
    expect(firstVisibleTranscriptItem(rendered, 320)).toBe(6)
    expect(firstVisibleTranscriptItem(rendered, 500)).toBeNull()
  })

  test('selects the turn containing the first actually visible item', () => {
    const turns = [
      { itemIndex: 0 },
      { itemIndex: 5 },
      { itemIndex: 9 },
      { itemIndex: 14 },
    ]

    expect(activeNavigationTurn(turns, 5, false)).toBe(1)
    expect(activeNavigationTurn(turns, 8, false)).toBe(1)
    expect(activeNavigationTurn(turns, 9, false)).toBe(2)
    expect(activeNavigationTurn(turns, 9, true)).toBe(3)
  })
})

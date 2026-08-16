import { describe, expect, test } from 'bun:test'
import {
  advanceMarkdownVeil,
  applyMarkdownVeil,
  createMarkdownVeilState,
  type HastNode,
} from './markdown-veil'

describe('markdown streaming veil', () => {
  test('tracks appended chunks independently and settles them once', () => {
    const state = createMarkdownVeilState()
    expect(advanceMarkdownVeil(state, 'one ', true, 0)).toMatchObject([
      { start: 0, end: 4, durationMs: 400 },
    ])
    const chunks = advanceMarkdownVeil(state, 'one two', true, 100)
    expect(chunks.map(({ start, end }) => [start, end])).toEqual([[0, 4], [4, 7]])
    expect(advanceMarkdownVeil(state, 'one two', true, 600)).toEqual([])
  })

  test('accelerates a backed-up stream and bounds retained chunks', () => {
    const state = createMarkdownVeilState()
    advanceMarkdownVeil(state, 'a', true, 0)
    advanceMarkdownVeil(state, 'ab', true, 100)
    const chunks = advanceMarkdownVeil(state, 'abc', true, 310)
    expect(chunks.map(({ start, end }) => [start, end])).toEqual([[1, 2], [2, 3]])

    for (let index = 0; index < 40; index += 1) {
      advanceMarkdownVeil(state, `${state.previous}x`, true, 310)
    }
    expect(state.chunks).toHaveLength(32)
  })

  test('settled content becomes the next streaming baseline', () => {
    const state = createMarkdownVeilState()
    advanceMarkdownVeil(state, 'existing', false, 0)
    expect(advanceMarkdownVeil(state, 'existing plus', true, 100)).toMatchObject([
      { start: 8, end: 13 },
    ])
  })

  test('wraps only the appended part of a rendered text node', () => {
    const tree: HastNode = {
      type: 'root',
      children: [{
        type: 'element',
        tagName: 'p',
        children: [{
          type: 'text',
          value: 'hello world',
          position: { start: { offset: 0 }, end: { offset: 11 } },
        }],
      }],
    }
    applyMarkdownVeil(tree, [{ start: 6, end: 11, startedAt: 100, durationMs: 400 }], 100)
    const paragraph = tree.children![0]!
    expect(paragraph.children?.[0]).toMatchObject({ type: 'text', value: 'hello ' })
    expect(paragraph.children?.[1]).toMatchObject({
      type: 'element',
      tagName: 'span',
      properties: { className: ['markdown-stream-veil'] },
      children: [{ type: 'text', value: 'world' }],
    })
  })
})

import { describe, expect, test } from 'bun:test'

import { modelMatchesSearch } from './model-search'
import { sessionStatusIsBusy } from './session-status'

describe('model search', () => {
  const model = {
    name: 'Claude Sonnet 4.5',
    id: 'claude-sonnet-4-5',
    sub_provider: 'Anthropic',
  }

  test('matches every token of the query, in any order', () => {
    expect(modelMatchesSearch(model, 'Claude', 'claude sonnet')).toBe(true)
    expect(modelMatchesSearch(model, 'Claude', 'sonnet claude')).toBe(true)
    expect(modelMatchesSearch(model, 'Claude', 'claude gpt')).toBe(false)
  })

  test('searches the provider and the sub-provider too', () => {
    expect(modelMatchesSearch(model, 'Claude Code', 'code')).toBe(true)
    expect(modelMatchesSearch(model, 'Claude Code', 'anthropic')).toBe(true)
    expect(modelMatchesSearch(model, 'Claude Code', 'openai')).toBe(false)
  })

  test('an empty query matches every model', () => {
    expect(modelMatchesSearch(model, 'Claude', '   ')).toBe(true)
  })
})

describe('session status', () => {
  test('a failed session is not busy', () => {
    expect(sessionStatusIsBusy('idle')).toBe(false)
    expect(sessionStatusIsBusy('failed')).toBe(false)
  })

  test('a running or parked session is busy', () => {
    expect(sessionStatusIsBusy('connecting')).toBe(true)
    expect(sessionStatusIsBusy('working')).toBe(true)
    expect(sessionStatusIsBusy('waiting')).toBe(true)
    expect(sessionStatusIsBusy('background')).toBe(true)
  })
})

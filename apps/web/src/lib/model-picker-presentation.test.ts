import { describe, expect, test } from 'bun:test'
import type { ProviderModel } from '@waku/client'
import {
  modelPickerProviderState,
  nextModelPickerHighlight,
  selectedModelPickerIndex,
  type ModelPickerRow,
} from './model-picker-presentation'

const model = (id: string): ProviderModel => ({
  id,
  name: id,
  is_default: false,
  reasoning_efforts: [],
  service_tiers: [],
  context_windows: [],
})

describe('model picker presentation', () => {
  test('keeps a cold provider selectable until its own probe resolves', () => {
    const state = { current: false, restricted: false, installed: undefined, isPending: true }
    expect(modelPickerProviderState(state)).toEqual({ enabled: true, pending: true })
    expect(modelPickerProviderState({ ...state, installed: true, isPending: false }))
      .toEqual({ enabled: true, pending: false })
    expect(modelPickerProviderState({ ...state, installed: false, isPending: false }))
      .toEqual({ enabled: false, pending: false })
    // A failed query is no longer pending; do not leave it checking forever.
    expect(modelPickerProviderState({ ...state, isPending: false }))
      .toEqual({ enabled: false, pending: false })
  })

  test('preserves restrictions, current sessions and cached probe results', () => {
    const state = { current: false, restricted: true, installed: undefined, isPending: true }
    expect(modelPickerProviderState(state)).toEqual({ enabled: false, pending: false })
    expect(modelPickerProviderState({ ...state, current: true }))
      .toEqual({ enabled: true, pending: true })
    expect(modelPickerProviderState({ ...state, restricted: false, installed: true }))
      .toEqual({ enabled: true, pending: false })
    expect(modelPickerProviderState({ ...state, restricted: false, installed: false }))
      .toEqual({ enabled: false, pending: false })
    expect(modelPickerProviderState({ ...state, current: true, installed: false, isPending: false }))
      .toEqual({ enabled: true, pending: false })
  })

  test('finds the selected model instead of treating the first row as selected', () => {
    const rows: ModelPickerRow[] = [
      { provider: 'claude', model: model('claude-fable-5') },
      { provider: 'claude', model: model('claude-opus-5') },
      { provider: 'claude', model: model('claude-opus-4-8') },
    ]

    expect(selectedModelPickerIndex(rows, 'claude', 'claude-opus-4-8')).toBe(2)
    expect(selectedModelPickerIndex(rows, 'claude', 'missing')).toBe(-1)
  })

  test('starts keyboard navigation only after an arrow key', () => {
    expect(nextModelPickerHighlight(null, 4, 'next')).toBe(0)
    expect(nextModelPickerHighlight(null, 4, 'previous')).toBe(3)
    expect(nextModelPickerHighlight(3, 4, 'next')).toBe(0)
  })
})

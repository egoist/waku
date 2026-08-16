import { describe, expect, test } from 'bun:test'
import type { ProviderModel } from '@waku/client'
import {
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

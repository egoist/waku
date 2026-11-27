import { describe, expect, test } from 'bun:test'
import type { ProviderModel, ProviderModelOption } from './generated'
import {
  catalogModel,
  cursorSuffixReasoningEffort,
  cursorSuffixServiceTier,
  resolveCursorModel,
} from './model-catalog'

function model(id: string, name = id): ProviderModel {
  return {
    id,
    name,
    is_default: false,
    reasoning_efforts: [],
    service_tiers: [],
    context_windows: [],
  }
}

describe('resolveCursorModel', () => {
  test('resolves CLI and ACP spellings and strips variant suffixes', () => {
    expect(resolveCursorModel(['default', 'grok-4.6', 'composer-2.5'], 'auto')).toEqual({
      value: 'default',
      suffix: '',
    })
    expect(resolveCursorModel(['auto', 'grok-4.6'], 'default')?.value).toBe('auto')
    expect(
      resolveCursorModel(
        ['default', 'grok-4.6', 'composer-2.5', 'claude-sonnet-4-6'],
        'cursor-grok-4.6-xhigh-fast',
      ),
    ).toEqual({
      value: 'grok-4.6',
      suffix: 'xhigh-fast',
    })
    expect(
      resolveCursorModel(['default', 'claude-sonnet-4-6'], 'claude-4.6-sonnet-medium-thinking'),
    ).toEqual({
      value: 'claude-sonnet-4-6',
      suffix: 'medium-thinking',
    })
  })

  test('prefers the longest advertised hyphen prefix', () => {
    expect(
      resolveCursorModel(['grok', 'grok-4.6'], 'grok-4.6-xhigh-fast'),
    ).toEqual({
      value: 'grok-4.6',
      suffix: 'xhigh-fast',
    })
  })
})

describe('catalogModel', () => {
  test('keeps exact catalogued exploded Cursor ids with an empty suffix', () => {
    const models = [
      model('auto', 'Auto'),
      model('claude-opus-5-thinking-high', 'Opus 5 Thinking'),
    ]
    const matched = catalogModel('cursor', models, 'claude-opus-5-thinking-high')
    expect(matched?.model.id).toBe('claude-opus-5-thinking-high')
    expect(matched?.suffix).toBe('')
  })

  test('resolves suffixed Cursor ids against base catalog entries only for Cursor', () => {
    const models = [model('claude-sonnet-4-6', 'Claude Sonnet 4.6')]
    expect(catalogModel('cursor', models, 'claude-4.6-sonnet-high-fast')).toEqual({
      model: models[0]!,
      suffix: 'high-fast',
    })
    expect(catalogModel('claude', models, 'claude-4.6-sonnet-high-fast')).toBeUndefined()
  })
})

describe('Cursor suffix traits', () => {
  const efforts: ProviderModelOption[] = [
    { id: 'low', label: 'Low' },
    { id: 'high', label: 'High' },
    { id: 'xhigh', label: 'Extra High' },
  ]
  const tiers: ProviderModelOption[] = [{ id: 'fast', label: 'Fast' }]

  test('prefers extra-high over high and detects fast tier', () => {
    expect(cursorSuffixReasoningEffort('thinking-extra-high-fast', efforts)).toBe('xhigh')
    expect(cursorSuffixServiceTier('thinking-extra-high-fast', tiers)).toBe('fast')
    expect(cursorSuffixServiceTier('thinking-high', tiers)).toBeUndefined()
  })
})
